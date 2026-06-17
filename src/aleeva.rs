use std::{
    sync::{
        Mutex,
        mpsc::{self, Sender},
    },
    thread,
};

use serde::Deserialize;

use crate::{common::WorkerMessage, settings::Settings};

const API_BASE: &str = "https://api.aleeva.io";

thread_local! {
    static CLIENT: ureq::Agent = ureq::agent();
}

/// A Discord server or channel as returned by the Aleeva API.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiscordId {
    pub id: String,
    pub name: String,
}

/// A server together with its (lazily fetched) channels.
#[derive(Debug, Clone, Default)]
pub struct Server {
    pub id: String,
    pub name: String,
    pub channels: Vec<DiscordId>,
}

/// Ephemeral (non-persisted) Aleeva runtime state shared between the worker
/// thread and the render thread. The API key itself lives in [`Settings`].
#[derive(Debug, Clone)]
pub struct AleevaState {
    pub authorised: bool,
    pub verifying: bool,
    pub servers: Vec<Server>,
    pub last_error: Option<String>,
}

impl AleevaState {
    const fn new() -> Self {
        Self {
            authorised: false,
            verifying: false,
            servers: Vec::new(),
            last_error: None,
        }
    }
}

static STATE: Mutex<AleevaState> = Mutex::new(AleevaState::new());
static COMMAND_TX: Mutex<Option<Sender<AleevaCommand>>> = Mutex::new(None);

#[derive(Debug)]
pub struct AleevaJob {
    pub index: usize,
    pub permalink: String,
    pub server_id: String,
    pub channel_id: String,
    pub send_notification: bool,
}

#[derive(Debug)]
pub enum AleevaCommand {
    /// Verify the configured API key and, if valid, fetch servers/channels.
    Verify,
    /// Fetch the channels for the given server id.
    FetchChannels(String),
    /// Post a dps.report permalink to Aleeva.
    Post(AleevaJob),
}

/// Returns a clone of the current Aleeva state for rendering.
pub fn snapshot() -> AleevaState {
    STATE.lock().unwrap().clone()
}

pub fn is_authorised() -> bool {
    STATE.lock().unwrap().authorised
}

/// Send a command to the Aleeva worker thread. No-op if the worker is not
/// running.
pub fn send(cmd: AleevaCommand) {
    if let Some(tx) = COMMAND_TX.lock().unwrap().as_ref() {
        if let Err(e) = tx.send(cmd) {
            log::error!("[Aleeva] Failed to send command to worker: {e}");
        }
    }
}

/// Drop the command sender so the worker thread terminates.
pub fn shutdown() {
    COMMAND_TX.lock().unwrap().take();
}

fn api_key() -> String {
    Settings::get().aleeva_api_key.clone()
}

pub fn run(out: Sender<WorkerMessage>) -> thread::JoinHandle<()> {
    let (tx, rx) = mpsc::channel::<AleevaCommand>();
    *COMMAND_TX.lock().unwrap() = Some(tx);

    thread::Builder::new()
        .name("aleeva-thread".to_string())
        .spawn(move || {
            for cmd in rx {
                CLIENT.with(|agent| match cmd {
                    AleevaCommand::Verify => handle_verify(agent),
                    AleevaCommand::FetchChannels(server_id) => {
                        handle_fetch_channels(agent, server_id)
                    }
                    AleevaCommand::Post(job) => {
                        let index = job.index;
                        let result = post_log(agent, job);
                        if let Err(e) = out.send(WorkerMessage::aleeva(index, result)) {
                            log::error!("[Aleeva] Failed to send result to main thread: {e}");
                        }
                    }
                });
            }
            log::trace!("[Aleeva] Worker thread exiting");
        })
        .expect("Could not create aleeva thread")
}

fn handle_verify(agent: &ureq::Agent) {
    {
        let mut st = STATE.lock().unwrap();
        st.verifying = true;
        st.last_error = None;
    }

    let result = (|| -> anyhow::Result<()> {
        if api_key().is_empty() {
            anyhow::bail!("No API key configured");
        }
        // A successful /server request validates the key.
        let servers = get_servers(agent)?;
        STATE.lock().unwrap().authorised = true;

        // Pick a default server if none selected yet.
        {
            let mut s = Settings::get();
            if s.aleeva_selected_server_id.is_empty() {
                if let Some(first) = servers.first() {
                    s.aleeva_selected_server_id = first.id.clone();
                }
            }
        }
        let selected = Settings::get().aleeva_selected_server_id.clone();

        let mut server_objs: Vec<Server> = servers
            .into_iter()
            .map(|d| Server {
                id: d.id,
                name: d.name,
                channels: Vec::new(),
            })
            .collect();

        // Fetch channels for the currently selected server for the dropdown.
        if !selected.is_empty() {
            let channels = get_channels(agent, &selected)?;
            {
                let mut s = Settings::get();
                if s.aleeva_selected_channel_id.is_empty() {
                    if let Some(first) = channels.first() {
                        s.aleeva_selected_channel_id = first.id.clone();
                    }
                }
            }
            if let Some(srv) = server_objs.iter_mut().find(|s| s.id == selected) {
                srv.channels = channels;
            }
        }

        STATE.lock().unwrap().servers = server_objs;
        Ok(())
    })();

    let mut st = STATE.lock().unwrap();
    st.verifying = false;
    if let Err(e) = result {
        log::error!("[Aleeva] API key verification failed: {e}");
        st.authorised = false;
        st.servers.clear();
        st.last_error = Some(format!("{e}"));
    }
}

fn handle_fetch_channels(agent: &ureq::Agent, server_id: String) {
    match get_channels(agent, &server_id) {
        Ok(channels) => {
            // Reset the selected channel to the first of the new server.
            {
                let mut s = Settings::get();
                s.aleeva_selected_channel_id =
                    channels.first().map(|c| c.id.clone()).unwrap_or_default();
            }
            let mut st = STATE.lock().unwrap();
            if let Some(srv) = st.servers.iter_mut().find(|s| s.id == server_id) {
                srv.channels = channels;
            }
        }
        Err(e) => {
            log::error!("[Aleeva] Failed to fetch channels for {server_id}: {e}");
            STATE.lock().unwrap().last_error = Some(format!("{e}"));
        }
    }
}

fn get_servers(agent: &ureq::Agent) -> anyhow::Result<Vec<DiscordId>> {
    let key = api_key();
    let resp = agent
        .get(&format!("{API_BASE}/server"))
        .set("Authorization", &format!("Bearer {key}"))
        .query("mode", "UPLOADS")
        .call()?;
    Ok(resp.into_json()?)
}

fn get_channels(agent: &ureq::Agent, server_id: &str) -> anyhow::Result<Vec<DiscordId>> {
    let key = api_key();
    let resp = agent
        .get(&format!("{API_BASE}/server/{server_id}/channel"))
        .set("Authorization", &format!("Bearer {key}"))
        .query("mode", "UPLOADS")
        .call()?;
    Ok(resp.into_json()?)
}

fn post_log(agent: &ureq::Agent, job: AleevaJob) -> anyhow::Result<bool> {
    log::info!("[Aleeva] Posting {}", job.permalink);
    let key = api_key();
    let body = serde_json::json!({
        "sendNotification": job.send_notification,
        "notificationServerId": job.server_id,
        "notificationChannelId": job.channel_id,
        "dpsReportPermalink": job.permalink,
    });
    let resp = agent
        .post(&format!("{API_BASE}/report"))
        .set("Authorization", &format!("Bearer {key}"))
        .set("Accept", "application/json")
        .send_json(body);
    match resp {
        Ok(_) => Ok(true),
        Err(ureq::Error::Status(status, res)) => {
            let body = res.into_string().unwrap_or_default();
            Err(anyhow::anyhow!("Aleeva post failed ({status}): {body}"))
        }
        Err(e) => Err(anyhow::anyhow!("Aleeva post failed: {e}")),
    }
}
