use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, RecvTimeoutError, Sender},
    thread,
    time::{Duration, Instant},
};

use anyhow::anyhow;
use serde::Deserialize;

use crate::common::{self, WorkerMessage, WvwParse, WvwUploadUpdate};

pub struct WvwUploadJob {
    pub index: usize,
    pub location: PathBuf,
    pub account: String,
    pub token: String,
}

thread_local! {
    static CLIENT: ureq::Agent = ureq::agent()
}

const TIMEOUT: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// The api prunes a finished ticket after 30 minutes, and a parse can run for
/// 15, so there is nothing left to learn past this.
const TICKET_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Deserialize)]
struct UploadResponse {
    result: bool,
    /// Handle for `GET /status/{ticket}`. 0 means the log was not enqueued.
    #[serde(default)]
    ticket: usize,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum TicketState {
    Queued,
    Processing,
    /// For a WvW log this means the parser output was stored, so the report
    /// exists. There is no wingman upload behind it.
    Uploaded,
    Skipped,
    Deferred,
    Failed {
        message: String,
    },
}

#[derive(Debug, Deserialize)]
struct TicketStatus {
    #[serde(default)]
    position: usize,
    #[serde(flatten)]
    state: TicketState,
}

/// A log whose parse we are still following.
struct Tracked {
    index: usize,
    ticket: usize,
    next_poll: Instant,
    deadline: Instant,
}

pub fn run(inc: Receiver<WvwUploadJob>, out: Sender<WorkerMessage>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("wvwupload-thread".to_string())
        .spawn(move || {
            let mut tracked: Vec<Tracked> = Vec::new();
            loop {
                match inc.recv_timeout(POLL_INTERVAL) {
                    Ok(job) => handle(job, &mut tracked, &out),
                    Err(RecvTimeoutError::Timeout) => {}
                    // The sender is gone, so no more logs are coming. The
                    // parses still running are the session service's problem
                    // from here; it polls them itself.
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                poll(&mut tracked, &out);
            }
        })
        .expect("Could not create wvwupload thread")
}

fn handle(job: WvwUploadJob, tracked: &mut Vec<Tracked>, out: &Sender<WorkerMessage>) {
    let index = job.index;
    match upload(&job) {
        Ok((id, ticket)) => {
            // The id goes out first: the session step waits on it, and it is
            // valid long before the parse behind it is done.
            send(out, index, Ok(WvwUploadUpdate::Uploaded(id)));
            if ticket == 0 {
                return;
            }
            let now = Instant::now();
            tracked.push(Tracked {
                index,
                ticket,
                next_poll: now,
                deadline: now + TICKET_TIMEOUT,
            });
        }
        Err(e) => {
            log::error!("[WvwUpload] {}: {e}", job.location.display());
            send(out, index, Err(e));
        }
    }
}

fn upload(job: &WvwUploadJob) -> anyhow::Result<(String, usize)> {
    log::info!("[WvwUpload] Uploading {}", job.location.display());

    let (content_type, data, _) =
        common::evtc_multipart(&job.location, &job.account, crate::WVW_BOSS_ID)?;

    let response = CLIENT.with(|c| {
        c.post(&format!("{}/wvw/evtc", common::EVTC_API_BASE))
            .set("Content-Type", &content_type)
            .set("Authorization", &format!("Bearer {}", job.token))
            .timeout(TIMEOUT)
            .send_bytes(&data)
            .map_err(describe)
    })?;

    let response: UploadResponse = response.into_json()?;
    if !response.result {
        return Err(anyhow!("the server did not accept the log"));
    }
    let id = response
        .id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| anyhow!("the server accepted the log but returned no id"))?;

    log::info!("[WvwUpload] {} -> {id}", job.location.display());
    Ok((id, response.ticket))
}

fn poll(tracked: &mut Vec<Tracked>, out: &Sender<WorkerMessage>) {
    tracked.retain_mut(|t| {
        if Instant::now() < t.next_poll {
            return true;
        }
        t.next_poll = Instant::now() + POLL_INTERVAL;
        if Instant::now() >= t.deadline {
            log::warn!("[WvwUpload] Gave up waiting on ticket {}", t.ticket);
            send(
                out,
                t.index,
                Ok(WvwUploadUpdate::Parse(WvwParse::Failed(
                    "gave up waiting for the parser".into(),
                ))),
            );
            return false;
        }
        match fetch_ticket(t.ticket) {
            Ok(Some(status)) => {
                let (parse, done) = match status.state {
                    TicketState::Queued => (
                        WvwParse::Queued {
                            position: status.position,
                        },
                        false,
                    ),
                    TicketState::Processing => (WvwParse::Parsing, false),
                    TicketState::Uploaded => (WvwParse::Parsed, true),
                    // Neither happens for a WvW log: both are verdicts about a
                    // wingman upload, which this job does not have.
                    TicketState::Skipped | TicketState::Deferred => (WvwParse::Parsing, false),
                    TicketState::Failed { message } => (WvwParse::Failed(message), true),
                };
                send(out, t.index, Ok(WvwUploadUpdate::Parse(parse)));
                !done
            }
            // Pruned, or never known. Either way there is nothing further to
            // report; the log itself is stored and the session still gets it.
            Ok(None) => {
                log::warn!("[WvwUpload] Ticket {} is unknown to the api", t.ticket);
                send(
                    out,
                    t.index,
                    Ok(WvwUploadUpdate::Parse(WvwParse::Failed(
                        "the parse ticket is no longer known to the server".into(),
                    ))),
                );
                false
            }
            Err(e) => {
                log::warn!("[WvwUpload] Failed to poll ticket {}: {e}", t.ticket);
                true
            }
        }
    });
}

/// `Ok(None)` means the api does not know the ticket (anymore).
fn fetch_ticket(ticket: usize) -> anyhow::Result<Option<TicketStatus>> {
    CLIENT.with(|c| {
        match c
            .get(&format!("{}/status/{ticket}", common::EVTC_API_BASE))
            .timeout(TIMEOUT)
            .call()
        {
            Ok(response) => Ok(Some(response.into_json()?)),
            Err(ureq::Error::Status(404, _)) => Ok(None),
            Err(e) => Err(describe(e)),
        }
    })
}

fn send(out: &Sender<WorkerMessage>, index: usize, update: anyhow::Result<WvwUploadUpdate>) {
    if let Err(e) = out.send(WorkerMessage::wvw_upload(index, update)) {
        log::error!("[WvwUpload] Failed to send result to main thread: {e}");
    }
}

fn describe(err: ureq::Error) -> anyhow::Error {
    match err {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            let body = body.trim();
            if body.is_empty() {
                anyhow!("HTTP {status}")
            } else {
                anyhow!("{body}")
            }
        }
        ureq::Error::Transport(t) => anyhow!("{t}"),
    }
}
