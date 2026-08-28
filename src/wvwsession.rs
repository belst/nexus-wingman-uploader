use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener},
    sync::{
        Arc, Mutex,
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

use chrono::{DateTime, Local, TimeDelta, Utc};
use mio::{Events, Interest, Poll, Token, Waker, net::TcpListener as MioListener};
use serde::Deserialize;

use crate::{common::WorkerMessage, settings, settings::Settings};

const CALLBACK_PATH: &str = "/oauth/callback";

const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

thread_local! {
    static CLIENT: ureq::Agent = ureq::agent();
}

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct ActiveSession {
    pub slug: String,
    pub name: Option<String>,
    pub started_at: DateTime<Utc>,
    pub last_upload: Option<Instant>,
    pub last_error: Option<String>,
    // Outcome per log index. Keyed so a retry of the same log replaces its
    // earlier failure instead of counting twice.
    outcomes: HashMap<usize, bool>,
}

impl ActiveSession {
    fn new(slug: String, name: Option<String>, started_at: Option<DateTime<Utc>>) -> Self {
        Self {
            slug,
            name,
            started_at: started_at.unwrap_or_else(Utc::now),
            last_upload: None,
            last_error: None,
            outcomes: HashMap::new(),
        }
    }

    /// Logs handed over, successful or not.
    pub fn logs(&self) -> usize {
        self.outcomes.len()
    }

    pub fn sent(&self) -> usize {
        self.outcomes.values().filter(|ok| **ok).count()
    }

    pub fn has_failures(&self) -> bool {
        self.outcomes.values().any(|ok| !ok)
    }

    pub fn elapsed(&self) -> TimeDelta {
        Utc::now() - self.started_at
    }
}

#[derive(Debug, Clone)]
pub struct SessionState {
    pub current: Option<ActiveSession>,
    pub busy: bool,
    pub last_error: Option<String>,
    pub account: Option<String>,
    pub providers: Vec<ProviderInfo>,
    pub history: Vec<PastSession>,
    pub history_loaded: bool,
}

const HISTORY_LIMIT: usize = 25;

#[derive(Debug, Clone, Deserialize)]
pub struct PastSession {
    pub slug: String,
    #[serde(default)]
    pub name: Option<String>,
    pub state: SessionLifecycle,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionLifecycle {
    Open,
    Combining,
    Ready,
    Failed,
    #[serde(other)]
    Unknown,
}

impl PastSession {
    pub fn label(&self) -> String {
        match self
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            Some(name) => name.to_owned(),
            None => self
                .started_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub label: String,
}

impl SessionState {
    const fn new() -> Self {
        Self {
            current: None,
            busy: false,
            last_error: None,
            account: None,
            providers: Vec::new(),
            history: Vec::new(),
            history_loaded: false,
        }
    }
}

static STATE: Mutex<SessionState> = Mutex::new(SessionState::new());
static COMMAND_TX: Mutex<Option<Sender<SessionCommand>>> = Mutex::new(None);

#[derive(Debug)]
pub enum SessionCommand {
    FetchProviders,
    Login {
        provider: String,
    },
    LoggedOut,
    Resume,
    FetchHistory,
    Start {
        name: Option<String>,
    },
    Stop,
    /// `log_id` is what `POST /wvw/evtc` answered with. The session service
    /// reads the parsed log from evtc.bel.st by that id.
    AddLog {
        index: usize,
        log_id: String,
        /// The session that was recording when the log was produced. A retry
        /// must not land in whatever session happens to be running later.
        slug: String,
    },
}

pub fn snapshot() -> SessionState {
    STATE.lock().unwrap().clone()
}

pub fn is_active() -> bool {
    STATE.lock().unwrap().current.is_some()
}

pub fn active() -> Option<ActiveSession> {
    STATE.lock().unwrap().current.clone()
}

// A stopped session is still combining somewhere in the history.
pub fn is_combining() -> bool {
    STATE
        .lock()
        .unwrap()
        .history
        .iter()
        .any(|s| s.state == SessionLifecycle::Combining)
}

pub fn active_slug() -> Option<String> {
    STATE
        .lock()
        .unwrap()
        .current
        .as_ref()
        .map(|s| s.slug.clone())
}

pub fn is_configured() -> bool {
    let settings = Settings::get();
    settings.enable_wvw_sessions && !settings.wvw_token.is_empty()
}

pub fn send(cmd: SessionCommand) {
    log::debug!("[Session] send({cmd:?})");
    let tx = COMMAND_TX.lock().unwrap();
    let Some(tx) = tx.as_ref() else {
        log::error!("[Session] Dropped {cmd:?}: the worker thread is not running");
        return;
    };
    if let Err(e) = tx.send(cmd) {
        log::error!("[Session] Failed to send command to worker: {e}");
    }
}

pub fn shutdown() {
    if let Some(waker) = LOGIN_WAKER.lock().unwrap().as_ref()
        && let Err(e) = waker.wake()
    {
        log::error!("[Session] Failed to cancel the pending login: {e}");
    }
    COMMAND_TX.lock().unwrap().take();
}

pub const BASE: &str = "https://wvw.bel.st";

/// Where a single parsed WvW log's report lives. Served by eiapi, which sits
/// behind the same host as the session frontend.
pub fn report_url(id: &str) -> String {
    format!("{BASE}/r/{id}")
}

fn token() -> String {
    Settings::get().wvw_token.clone()
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    slug: String,
    #[serde(default)]
    name: Option<String>,
    // Absent on servers that do not report it, and the elapsed time then
    // counts from when this client learned about the session.
    #[serde(default)]
    started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct MeResponse {
    display_name: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ApiError {
    error: String,
}

pub fn run(out: Sender<WorkerMessage>) -> thread::JoinHandle<()> {
    let (tx, rx) = mpsc::channel::<SessionCommand>();
    *COMMAND_TX.lock().unwrap() = Some(tx);

    thread::Builder::new()
        .name("session-thread".to_string())
        .spawn(move || {
            log::debug!("[Session] Worker thread started");
            let mut poll = CombinePoll::default();
            loop {
                // Waits forever unless a session is combining, in which case it
                // wakes up to refresh the history on its own.
                let cmd = match poll.next_at() {
                    Some(at) => match rx.recv_timeout(at.saturating_duration_since(Instant::now()))
                    {
                        Ok(cmd) => Some(cmd),
                        Err(RecvTimeoutError::Timeout) => None,
                        Err(RecvTimeoutError::Disconnected) => break,
                    },
                    None => match rx.recv() {
                        Ok(cmd) => Some(cmd),
                        Err(_) => break,
                    },
                };

                CLIENT.with(|agent| {
                    match cmd {
                        Some(cmd) => {
                            log::debug!("[Session] Handling {cmd:?}");
                            match cmd {
                                SessionCommand::FetchProviders => handle_fetch_providers(agent),
                                SessionCommand::Login { provider } => {
                                    handle_login(agent, &provider)
                                }
                                SessionCommand::LoggedOut => {
                                    let mut state = STATE.lock().unwrap();
                                    state.current = None;
                                    state.account = None;
                                    state.last_error = None;
                                    state.history.clear();
                                    state.history_loaded = false;
                                }
                                SessionCommand::Resume => handle_resume(agent),
                                SessionCommand::FetchHistory => handle_fetch_history(agent),
                                SessionCommand::Start { name } => handle_start(agent, name),
                                SessionCommand::Stop => handle_stop(agent),
                                SessionCommand::AddLog {
                                    index,
                                    log_id,
                                    slug,
                                } => {
                                    handle_add_log(agent, &out, index, &log_id, &slug);
                                }
                            }
                        }
                        None => {
                            log::debug!("[Session] Polling for the combined report");
                            handle_fetch_history(agent);
                        }
                    }
                    poll.update(is_combining());
                });
            }
            log::trace!("[Session] Worker thread exiting");
        })
        .expect("Could not create session thread")
}

#[derive(Debug, Default)]
struct CombinePoll {
    next_at: Option<Instant>,
    interval: Option<Duration>,
    give_up_at: Option<Instant>,
}

const POLL_MIN: Duration = Duration::from_secs(5);
const POLL_MAX: Duration = Duration::from_secs(30);
// Server limit 30 minutes
const POLL_LIMIT: Duration = Duration::from_secs(35 * 60);

impl CombinePoll {
    fn next_at(&self) -> Option<Instant> {
        self.next_at
    }

    fn update(&mut self, combining: bool) {
        if !combining {
            *self = Self::default();
            return;
        }
        let now = Instant::now();
        let give_up_at = *self.give_up_at.get_or_insert(now + POLL_LIMIT);
        if now >= give_up_at {
            log::warn!("[Session] Still combining after {POLL_LIMIT:?}, no longer polling");
            self.next_at = None;
            return;
        }
        let interval = self
            .interval
            .map_or(POLL_MIN, |i| (i * 2).min(POLL_MAX))
            .min(POLL_MAX);
        self.interval = Some(interval);
        self.next_at = Some(now + interval);
    }
}

fn handle_add_log(
    agent: &ureq::Agent,
    out: &Sender<WorkerMessage>,
    index: usize,
    log_id: &str,
    slug: &str,
) {
    let result = add_log(agent, log_id, slug);
    {
        let mut state = STATE.lock().unwrap();
        // Never book the outcome onto a session this log does not belong to.
        if let Some(session) = state.current.as_mut().filter(|s| s.slug == slug) {
            session.outcomes.insert(index, result.is_ok());
            session.last_upload = Some(Instant::now());
            session.last_error = result.as_ref().err().map(|e| e.to_string());
        }
    }
    if let Err(e) = &result {
        log::error!("[Session] {log_id}: {e}");
    }
    if let Err(e) = out.send(WorkerMessage::session(index, result.map(|()| true))) {
        log::error!("[Session] Failed to send result to main thread: {e}");
    }
}

fn set_error(message: impl Into<String>) {
    let message = message.into();
    log::error!("[Session] {message}");
    let mut state = STATE.lock().unwrap();
    state.busy = false;
    state.last_error = Some(message);
}

// Prefer the API's own `{"error": "..."}` message over "HTTP 409".
fn describe(err: ureq::Error) -> String {
    match err {
        ureq::Error::Status(status, response) => {
            let body = response.into_string().unwrap_or_default();
            match serde_json::from_str::<ApiError>(&body) {
                Ok(api) if !api.error.is_empty() => api.error,
                _ => format!("HTTP {status}"),
            }
        }
        ureq::Error::Transport(t) => format!("{t}"),
    }
}

fn handle_fetch_providers(agent: &ureq::Agent) {
    let url = format!("{BASE}/api/auth/providers");
    log::debug!("[Session] GET {url}");
    match agent
        .get(&url)
        .timeout(TIMEOUT)
        .call()
        .map_err(describe)
        .and_then(|r| {
            r.into_json::<Vec<ProviderInfo>>()
                .map_err(|e| e.to_string())
        }) {
        Ok(providers) => {
            log::debug!("[Session] Login providers: {providers:?}");
            STATE.lock().unwrap().providers = providers;
        }
        Err(e) => log::warn!("[Session] Could not fetch login providers: {e}"),
    }
}

const CALLBACK_PAGE: &str = "<h1>Signed in</h1>\
    <p>You can close this tab and return to the game.</p>";

// temp server on loopback, does token exchange after getting the code.
fn handle_login(agent: &ureq::Agent, provider: &str) {
    log::info!("[Session] Starting login with provider {provider}");
    STATE.lock().unwrap().busy = true;

    // Port 0 lets the OS pick a free one. RFC 8252 allows any loopback port.
    let listener = match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))) {
        Ok(listener) => listener,
        Err(e) => return set_error(format!("Could not open a local port: {e}")),
    };
    let port = match listener.local_addr() {
        Ok(addr) => addr.port(),
        Err(e) => return set_error(format!("Could not read the local port: {e}")),
    };
    let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    log::debug!("[Session] Listening for the callback on {redirect_uri}");

    let start_url = format!("{BASE}/api/auth/start");
    log::debug!("[Session] POST {start_url}");
    let started: StartResponse = match agent
        .post(&start_url)
        .timeout(TIMEOUT)
        .send_json(serde_json::json!({
            "provider": provider,
            "redirect_uri": redirect_uri,
        }))
        .map_err(describe)
        .and_then(|r| r.into_json::<StartResponse>().map_err(|e| e.to_string()))
    {
        Ok(started) => started,
        Err(e) => return set_error(e),
    };

    // Logged in full so the URL can be pasted by hand if the browser fails.
    log::info!("[Session] Opening the browser at {}", started.authorize_url);
    if let Err(e) = open::that_detached(&started.authorize_url) {
        return set_error(format!("Could not open the browser: {e}"));
    }
    log::debug!("[Session] Browser launched, waiting up to {LOGIN_TIMEOUT:?} for the callback");

    let callback = match wait_for_callback(listener) {
        Ok(callback) => callback,
        Err(e) => return set_error(e),
    };
    log::debug!(
        "[Session] Callback received (code {} chars, iss {:?})",
        callback.code.len(),
        callback.iss
    );
    if callback.state != started.state {
        return set_error("The sign in reply did not match the request".to_string());
    }

    let mut body = serde_json::json!({
        "state": callback.state,
        "code": callback.code,
    });
    if let Some(iss) = callback.iss {
        body["iss"] = iss.into();
    }

    let complete_url = format!("{BASE}/api/auth/complete");
    log::debug!("[Session] POST {complete_url}");
    let completed: CompleteResponse = match agent
        .post(&complete_url)
        .timeout(TIMEOUT)
        .send_json(body)
        .map_err(describe)
        .and_then(|r| r.into_json::<CompleteResponse>().map_err(|e| e.to_string()))
    {
        Ok(completed) => completed,
        Err(e) => return set_error(e),
    };

    {
        let mut settings = Settings::get();
        settings.wvw_token = completed.token;
        // Persist now, the user cannot re-enter a token they never see.
        if let Err(e) = settings.store(settings::config_path()) {
            log::error!("[Session] Failed to store the token: {e}");
        }
    }
    {
        let mut state = STATE.lock().unwrap();
        state.busy = false;
        state.last_error = None;
        state.account = Some(completed.user.display_name);
    }
    log::info!("[Session] Signed in");

    handle_resume(agent);
}

#[derive(Debug)]
struct Callback {
    code: String,
    state: String,
    iss: Option<String>,
}

const SERVER: Token = Token(0);
const SHUTDOWN: Token = Token(1);

static LOGIN_WAKER: Mutex<Option<Arc<Waker>>> = Mutex::new(None);

fn wait_for_callback(listener: TcpListener) -> Result<Callback, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("Could not configure the local port: {e}"))?;

    let mut poll = Poll::new().map_err(|e| format!("Could not create the poll: {e}"))?;
    let waker = Arc::new(
        Waker::new(poll.registry(), SHUTDOWN)
            .map_err(|e| format!("Could not create the waker: {e}"))?,
    );
    *LOGIN_WAKER.lock().unwrap() = Some(waker);

    let result = poll_for_callback(&mut poll, listener);
    LOGIN_WAKER.lock().unwrap().take();
    result
}

fn poll_for_callback(poll: &mut Poll, listener: TcpListener) -> Result<Callback, String> {
    let mut listener = MioListener::from_std(listener);
    poll.registry()
        .register(&mut listener, SERVER, Interest::READABLE)
        .map_err(|e| format!("Could not register the local port: {e}"))?;

    let mut events = Events::with_capacity(8);
    let deadline = std::time::Instant::now() + LOGIN_TIMEOUT;

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err("Sign in timed out".to_string());
        }
        if let Err(e) = poll.poll(&mut events, Some(remaining)) {
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("Could not wait for the sign in reply: {e}"));
        }

        for event in events.iter() {
            match event.token() {
                SHUTDOWN => return Err("Sign in cancelled".to_string()),
                SERVER => {
                    // The browser may open several connections.
                    while let Ok((stream, peer)) = listener.accept() {
                        log::debug!("[Session] Callback connection from {peer}");
                        if let Some(result) = handle_connection(stream.into()) {
                            return result;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn handle_connection(mut stream: std::net::TcpStream) -> Option<Result<Callback, String>> {
    // The socket inherits non-blocking from the listener. Timeouts keep a
    // stalled client from parking the worker.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let mut line = String::new();
    if let Err(e) = BufReader::new(&mut stream).read_line(&mut line) {
        log::debug!("[Session] Could not read the callback request: {e}");
        return None;
    }

    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    if method != "GET" {
        log::debug!("[Session] Ignoring a {method} request on the callback port");
        respond(&mut stream, 405, "<h1>Method not allowed</h1>");
        return None;
    }
    let (path, _) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        log::debug!("[Session] Ignoring a request for {path}, expected {CALLBACK_PATH}");
        respond(&mut stream, 404, "<h1>Not found</h1>");
        return None;
    }

    let result = parse_callback(target);
    // Answer either way so the browser does not hang on a spinner.
    match &result {
        Ok(_) => respond(&mut stream, 200, CALLBACK_PAGE),
        Err(e) => respond(
            &mut stream,
            400,
            &format!("<h1>Sign in failed</h1><p>{}</p>", html_escape(e)),
        ),
    }
    Some(result)
}

// callback query is external and could be anything. escape it.
fn html_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn respond(stream: &mut std::net::TcpStream, status: u16, body: &str) {
    let page = format!(
        r#"<!doctype html><meta charset=utf-8><meta name="color-scheme" content="dark"><title>Log Uploader</title>
         <body style="font-family:sans-serif;text-align:center;padding-top:3em">{body}"#
    );
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        )
        .as_bytes(),
    );
    let _ = stream.flush();
}

fn parse_callback(target: &str) -> Result<Callback, String> {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if path != CALLBACK_PATH {
        return Err(format!("Unexpected request for {path}"));
    }

    let mut code = None;
    let mut state = None;
    let mut iss = None;
    let mut error = None;
    let mut description = None;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let value = percent_decode(value);
        match key {
            "code" => code = Some(value),
            "state" => state = Some(value),
            "iss" => iss = Some(value),
            "error" => error = Some(value),
            "error_description" => description = Some(value),
            _ => {}
        }
    }

    if let Some(error) = error {
        let detail = description.unwrap_or_default();
        return Err(format!("{error} {detail}").trim().to_string());
    }
    match (code, state) {
        (Some(code), Some(state)) => Ok(Callback { code, state, iss }),
        _ => Err("The sign in reply was incomplete".to_string()),
    }
}

fn percent_decode(value: &str) -> String {
    let bytes = value.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Deserialize)]
struct StartResponse {
    authorize_url: String,
    state: String,
}

#[derive(Debug, Deserialize)]
struct CompleteResponse {
    token: String,
    user: MeResponse,
}

fn handle_resume(agent: &ureq::Agent) {
    if !is_configured() {
        return;
    }
    STATE.lock().unwrap().busy = true;

    match agent
        .get(&format!("{BASE}/api/me"))
        .set("Authorization", &format!("Bearer {}", token()))
        .timeout(TIMEOUT)
        .call()
    {
        Ok(response) => match response.into_json::<MeResponse>() {
            Ok(me) => STATE.lock().unwrap().account = Some(me.display_name),
            Err(e) => log::warn!("[Session] Could not read the account: {e}"),
        },
        Err(e) => {
            set_error(format!("Not logged in: {}", describe(e)));
            return;
        }
    }

    match agent
        .get(&format!("{BASE}/api/sessions/current"))
        .set("Authorization", &format!("Bearer {}", token()))
        .timeout(TIMEOUT)
        .call()
    {
        Ok(response) => {
            // `null` when nothing is open.
            let current = response
                .into_json::<Option<SessionResponse>>()
                .unwrap_or(None);
            let mut state = STATE.lock().unwrap();
            state.busy = false;
            state.last_error = None;
            state.current = current.map(|s| ActiveSession::new(s.slug, s.name, s.started_at));
            if let Some(session) = state.current.as_ref() {
                log::info!("[Session] Resumed {}", session.slug);
            }
        }
        Err(e) => {
            set_error(describe(e));
            return;
        }
    }

    handle_fetch_history(agent);
}

// Failures only warn. A flaky history request must not bury a real error about
// the recording session.
fn handle_fetch_history(agent: &ureq::Agent) {
    if !is_configured() {
        return;
    }

    let url = format!("{BASE}/api/sessions?limit={HISTORY_LIMIT}");
    log::debug!("[Session] GET {url}");
    match agent
        .get(&url)
        .set("Authorization", &format!("Bearer {}", token()))
        .timeout(TIMEOUT)
        .call()
        .map_err(describe)
        .and_then(|r| r.into_json::<Vec<PastSession>>().map_err(|e| e.to_string()))
    {
        Ok(sessions) => {
            // The open session is already shown by the buttons above the list.
            let history: Vec<_> = sessions
                .into_iter()
                .filter(|s| s.state != SessionLifecycle::Open)
                .collect();
            log::debug!("[Session] {} finished sessions", history.len());
            let mut state = STATE.lock().unwrap();
            state.history = history;
            state.history_loaded = true;
        }
        Err(e) => log::warn!("[Session] Could not fetch the session history: {e}"),
    }
}

fn handle_start(agent: &ureq::Agent, name: Option<String>) {
    STATE.lock().unwrap().busy = true;

    let body = serde_json::json!({ "name": name });
    match agent
        .post(&format!("{BASE}/api/sessions"))
        .set("Authorization", &format!("Bearer {}", token()))
        .timeout(TIMEOUT)
        .send_json(body)
    {
        Ok(response) => match response.into_json::<SessionResponse>() {
            Ok(session) => {
                log::info!("[Session] Started {}", session.slug);
                let mut state = STATE.lock().unwrap();
                state.busy = false;
                state.last_error = None;
                state.current = Some(ActiveSession::new(
                    session.slug,
                    session.name,
                    session.started_at,
                ));
            }
            Err(e) => set_error(format!("Unexpected response: {e}")),
        },
        Err(e) => set_error(describe(e)),
    }
}

fn handle_stop(agent: &ureq::Agent) {
    let Some(slug) = active_slug() else {
        return;
    };
    STATE.lock().unwrap().busy = true;

    match agent
        .post(&format!("{BASE}/api/sessions/{slug}/stop"))
        .set("Authorization", &format!("Bearer {}", token()))
        .timeout(TIMEOUT)
        .call()
    {
        Ok(response) => {
            // 204 means no logs were collected and the server discarded it.
            if response.status() == 204 {
                log::info!("[Session] {slug} was empty and has been discarded");
            } else {
                log::info!("[Session] Stopped {slug}, the report is being combined");
            }
            let mut state = STATE.lock().unwrap();
            state.busy = false;
            state.last_error = None;
            state.current = None;
        }
        Err(e) => {
            // Only a verdict from the server means the session is gone there.
            // A transport failure most likely never reached it, so keep the
            // session so the user can press stop again.
            let released = matches!(&e, ureq::Error::Status(404 | 409 | 410, _));
            let message = describe(e);
            let mut state = STATE.lock().unwrap();
            state.busy = false;
            state.last_error = Some(message.clone());
            if released {
                state.current = None;
                log::error!("[Session] Stop failed but the session was released: {message}");
            } else {
                log::error!("[Session] Stop failed, the session is still recording: {message}");
            }
        }
    }

    handle_fetch_history(agent);
}

fn add_log(agent: &ureq::Agent, log_id: &str, slug: &str) -> anyhow::Result<()> {
    if active_slug().as_deref() != Some(slug) {
        anyhow::bail!("the session {slug} is no longer recording");
    }

    agent
        .post(&format!("{BASE}/api/sessions/{slug}/logs"))
        .set("Authorization", &format!("Bearer {}", token()))
        .timeout(TIMEOUT)
        .send_json(serde_json::json!({ "log_id": log_id }))
        .map_err(|e| anyhow::anyhow!("{}", describe(e)))?;

    Ok(())
}
