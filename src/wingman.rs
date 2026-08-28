use anyhow::anyhow;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

use crate::common::{WingmanProgress, WingmanUpdate, WorkerMessage};

pub type WingmanJob = (usize, PathBuf, String, u16);

// const API_BASE: &str = "https://gw2wingman.nevermindcreations.de";
use crate::common::EVTC_API_BASE as API_BASE;
const WINGMAN_BASE: &str = "https://gw2wingman.nevermindcreations.de";

const TICKET_POLL_INTERVAL: Duration = Duration::from_secs(3);
const CHECK_POLL_INTERVAL: Duration = Duration::from_secs(5);
const TICKET_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const WINGMAN_CHECK_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Deserialize)]
struct EvtcResponse {
    result: bool,
    /// Handle for `GET /status/{ticket}`. 0 means the log was not enqueued.
    #[serde(default)]
    ticket: usize,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum TicketState {
    Queued,
    Processing,
    Uploaded,
    Skipped,
    Deferred { message: String, attempt: u32 },
    Failed { message: String },
}

#[derive(Debug, Deserialize)]
struct TicketStatus {
    #[serde(default)]
    position: usize,
    #[serde(flatten)]
    state: TicketState,
}

#[derive(Debug, Deserialize)]
struct CheckLog {
    /// Slug of the published log, the permalink is `{WINGMAN_BASE}/log/{html}`.
    html: String,
}

#[derive(Debug, Deserialize)]
struct CheckResponse {
    success: bool,
    #[serde(default)]
    log: Option<CheckLog>,
    #[serde(default)]
    error: Option<String>,
}

enum Phase {
    /// Enqueued at the api, polling `GET /status/{ticket}`.
    Ticket(usize),
    /// Handed to wingman, polling wingman for the permalink.
    Check,
}

struct Tracked {
    index: usize,
    remote_name: String,
    filesize: String,
    account: String,
    boss_id: String,
    // State machine to track progress
    phase: Phase,
    next_poll: Instant,
    deadline: Instant,
    /// Last progress we told the ui about, so we only send on change.
    reported: Option<WingmanProgress>,
}

impl Tracked {
    fn enter_check(&mut self) {
        self.phase = Phase::Check;
        self.next_poll = Instant::now() + CHECK_POLL_INTERVAL;
        self.deadline = Instant::now() + WINGMAN_CHECK_TIMEOUT;
    }
}

thread_local! {
    static CLIENT: ureq::Agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(120))
        .build()
}

pub fn run(inc: Receiver<WingmanJob>, out: Sender<WorkerMessage>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("wingman-thread".to_string())
        .spawn(move || CLIENT.with(|c| worker(c, inc, out)))
        .expect("Could not create wingman thread")
}

fn worker(client: &ureq::Agent, inc: Receiver<WingmanJob>, out: Sender<WorkerMessage>) {
    let mut tracked: Vec<Tracked> = Vec::new();
    loop {
        let job = match tracked.iter().map(|t| t.next_poll).min() {
            // No tracked logs, so we wait for new work.
            None => inc.recv().ok(),
            // wait for new work but with timeout on the next poll
            Some(next) => match inc.recv_timeout(next.saturating_duration_since(Instant::now())) {
                Ok(job) => Some(job),
                Err(RecvTimeoutError::Timeout) => None,
                // Drop in flight and exit thread
                Err(RecvTimeoutError::Disconnected) => return,
            },
        };
        match job {
            // New log, upload then start polling
            Some(job) => accept(client, job, &mut tracked, &out),
            // Nothing was in flight and the channel closed.
            None if tracked.is_empty() => return,
            // A poll came due.
            None => {}
        }
        poll(client, &mut tracked, &out);
    }
}

fn send(out: &Sender<WorkerMessage>, index: usize, update: WingmanUpdate) {
    if let Err(e) = out.send(WorkerMessage::wingman(index, update)) {
        log::error!("[Wingman] Failed to send wingman result to main thread: {e}");
    }
}

fn progress(out: &Sender<WorkerMessage>, t: &mut Tracked, p: WingmanProgress) {
    if t.reported.as_ref() == Some(&p) {
        return;
    }
    t.reported = Some(p.clone());
    send(out, t.index, WingmanUpdate::Progress(p));
}

fn accept(
    client: &ureq::Agent,
    (index, location, account, boss_id): WingmanJob,
    tracked: &mut Vec<Tracked>,
    out: &Sender<WorkerMessage>,
) {
    send(
        out,
        index,
        WingmanUpdate::Progress(WingmanProgress::Uploading),
    );
    match upload(client, index, &location, &account, boss_id) {
        Ok(t) => tracked.push(t),
        Err(e) => send(out, index, WingmanUpdate::Error(e)),
    }
}

fn upload(
    client: &ureq::Agent,
    index: usize,
    location: &Path,
    account: &str,
    boss_id: u16,
) -> anyhow::Result<Tracked> {
    log::info!("[Wingman] Uploading {}", location.display());

    let file_name = crate::common::file_name(location)?;
    let (content_type, data, filesize) = crate::common::evtc_multipart(location, account, boss_id)?;

    let now = Instant::now();
    let mut tracked = Tracked {
        index,
        // api prevents collisions by prefixing with account name (dotless, wingman had a filename
        // parse bug where it split on dots)
        remote_name: format!("{}_{}", account.replace('.', ""), file_name),
        filesize,
        account: account.to_owned(),
        boss_id: boss_id.to_string(),
        phase: Phase::Check,
        next_poll: now,
        deadline: now + WINGMAN_CHECK_TIMEOUT,
        reported: None,
    };

    match client
        .post(&format!("{API_BASE}/evtc"))
        .set("Content-Type", &content_type)
        .send_bytes(&data)
    {
        Ok(resp) => {
            let resp: EvtcResponse = resp.into_json()?;
            if !resp.result || resp.ticket == 0 {
                return Err(anyhow!("api did not accept the log"));
            }
            tracked.phase = Phase::Ticket(resp.ticket);
            tracked.deadline = now + TICKET_TIMEOUT;
        }
        // Duplicate upload, wingman log might still exist
        Err(ureq::Error::Status(409, _)) => {
            log::info!("[Wingman] {} is a duplicate", tracked.remote_name);
        }
        Err(e) => return Err(e.into()),
    }

    Ok(tracked)
}

fn poll(client: &ureq::Agent, tracked: &mut Vec<Tracked>, out: &Sender<WorkerMessage>) {
    tracked.retain_mut(|t| {
        if Instant::now() < t.next_poll {
            return true;
        }
        let finished = match t.phase {
            Phase::Ticket(ticket) => poll_ticket(client, t, ticket, out),
            Phase::Check => poll_check(client, t, out),
        };
        !finished
    });
}

/// Returns true when log is done. (dont poll again)
fn poll_ticket(
    client: &ureq::Agent,
    t: &mut Tracked,
    ticket: usize,
    out: &Sender<WorkerMessage>,
) -> bool {
    t.next_poll = Instant::now() + TICKET_POLL_INTERVAL;
    if Instant::now() >= t.deadline {
        log::warn!("[Wingman] Gave up waiting on ticket {ticket}");
        t.enter_check();
        return false;
    }
    match fetch_ticket(client, ticket) {
        Ok(Some(status)) => match status.state {
            TicketState::Queued => {
                progress(
                    out,
                    t,
                    WingmanProgress::Queued {
                        position: status.position,
                    },
                );
                false
            }
            TicketState::Processing => {
                progress(out, t, WingmanProgress::Processing);
                false
            }
            TicketState::Deferred { message, attempt } => {
                log::warn!("[Wingman] Ticket {ticket} deferred (attempt {attempt}): {message}");
                progress(out, t, WingmanProgress::Deferred { attempt });
                false
            }
            TicketState::Uploaded => {
                t.enter_check();
                progress(out, t, WingmanProgress::Checking);
                false
            }
            TicketState::Skipped => {
                log::info!("[Wingman] Ticket {ticket} had nothing to upload");
                send(out, t.index, WingmanUpdate::Skipped);
                true
            }
            TicketState::Failed { message } => {
                send(out, t.index, WingmanUpdate::Error(anyhow!("{message}")));
                true
            }
        },
        // Pruned or lost. The upload itself went through, so look for the log
        // on wingman instead of failing the whole thing.
        Ok(None) => {
            log::warn!("[Wingman] Ticket {ticket} is unknown to the api");
            t.enter_check();
            false
        }
        // Transient, try again on the next tick.
        Err(e) => {
            log::warn!("[Wingman] Failed to poll ticket {ticket}: {e}");
            false
        }
    }
}

/// Returns true when log is done. (dont poll again)
fn poll_check(client: &ureq::Agent, t: &mut Tracked, out: &Sender<WorkerMessage>) -> bool {
    t.next_poll = Instant::now() + CHECK_POLL_INTERVAL;
    let expired = Instant::now() >= t.deadline;
    match fetch_permalink(client, t) {
        Ok(Some(url)) => {
            log::info!("[Wingman] {} published at {url}", t.remote_name);
            send(out, t.index, WingmanUpdate::Done(Some(url)));
            true
        }
        Ok(None) if expired => {
            // Wingman did not give us a link in time
            log::warn!("[Wingman] No permalink for {} in time", t.remote_name);
            send(out, t.index, WingmanUpdate::Done(None));
            true
        }
        Err(e) if expired => {
            send(out, t.index, WingmanUpdate::Error(e));
            true
        }
        Ok(None) => false,
        Err(e) => {
            log::warn!("[Wingman] Failed to check {}: {e}", t.remote_name);
            false
        }
    }
}

/// `Ok(None)` means the api does not know the ticket (anymore).
fn fetch_ticket(client: &ureq::Agent, ticket: usize) -> anyhow::Result<Option<TicketStatus>> {
    match client.get(&format!("{API_BASE}/status/{ticket}")).call() {
        Ok(resp) => Ok(Some(resp.into_json()?)),
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `Ok(None)` means wingman does not have the log yet.
fn fetch_permalink(client: &ureq::Agent, t: &Tracked) -> anyhow::Result<Option<String>> {
    let resp: CheckResponse = client
        .post(&format!("{WINGMAN_BASE}/checkUploadSuccessfulWithLog"))
        .send_form(&[
            ("file", t.remote_name.as_str()),
            ("filesize", t.filesize.as_str()),
            ("bossID", t.boss_id.as_str()),
            ("account", t.account.as_str()),
        ])?
        .into_json()?;

    if !resp.success {
        log::trace!(
            "[Wingman] {} not on wingman yet: {}",
            t.remote_name,
            resp.error.unwrap_or_default()
        );
        return Ok(None);
    }

    Ok(resp.log.map(|l| format!("{WINGMAN_BASE}/log/{}", l.html)))
}
