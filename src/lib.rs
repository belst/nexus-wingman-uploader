use std::{
    cell::{Cell, RefCell},
    collections::HashSet,
    ffi::CString,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        mpsc::{self, Receiver, Sender},
    },
    thread::{self},
    time::{Duration, Instant},
};

use arcdpslog::Step;
use common::*;
use filewatcher::ReceiverExt;
use nexus::{
    AddonFlags, UpdateProvider,
    gui::{RenderType, register_render},
    imgui::{
        ChildWindow, Condition, StyleColor, TableColumnFlags, TableColumnSetup, TableFlags, Ui,
        Window,
    },
    keybind::{Keybind, register_keybind_with_struct, unregister_keybind},
    keybind_handler,
    quick_access::{add_quick_access, remove_quick_access},
    render,
};
use notify::{Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use settings::Settings;
use util::e;

use crate::{
    events::{
        DpsReportEvent, EV_DPSREPORT, EV_LOG_DETECTED, EV_LOG_PARSED, EV_WINGMAN, LogDetectedEvent,
        LogParsedEvent, WingmanEvent,
    },
    settings::FRAME_NUM,
    util::ProgressBarIndeterminate,
};

mod aleeva;
mod arcdpslog;
mod assets;
mod common;
mod dpsreport;
mod events;
mod evtc;
mod filewatcher;
mod settings;
mod util;
mod wingman;
mod wvwsession;
mod wvwupload;

const WVW_BOSS_ID: u16 = 1;

struct State {
    producer_rx: Mutex<Option<Receiver<common::WorkerMessage>>>,
    evtc_worker: Mutex<Option<Sender<evtc::EvtcJob>>>,
    filewatcher: Mutex<Option<Box<dyn Watcher + Send>>>,
    file_rx: Mutex<Option<Receiver<Result<Event, notify::Error>>>>,
    dps_worker: Mutex<Option<Sender<dpsreport::DpsJob>>>,
    wingman_worker: Mutex<Option<Sender<wingman::WingmanJob>>>,
    wvw_upload_worker: Mutex<Option<Sender<wvwupload::WvwUploadJob>>>,
    threads: Mutex<Vec<thread::JoinHandle<()>>>,
    logs: Mutex<Vec<arcdpslog::Log>>,
}

impl State {
    // Returns the receiver for updates from the workers
    fn try_next_producer(&self) -> Option<common::WorkerMessage> {
        self.producer_rx
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
    }

    fn init_producer(&self) -> Sender<common::WorkerMessage> {
        let (tx, rx) = mpsc::channel();
        *self.producer_rx.lock().unwrap() = Some(rx);
        tx
    }

    fn init_evtc_worker(&self) -> Receiver<evtc::EvtcJob> {
        let (tx, rx) = mpsc::channel();
        *self.evtc_worker.lock().unwrap() = Some(tx);
        rx
    }

    fn init_dps_worker(&self) -> Receiver<dpsreport::DpsJob> {
        let (tx, rx) = mpsc::channel();
        *self.dps_worker.lock().unwrap() = Some(tx);
        rx
    }

    fn init_wingman_worker(&self) -> Receiver<wingman::WingmanJob> {
        let (tx, rx) = mpsc::channel();
        *self.wingman_worker.lock().unwrap() = Some(tx);
        rx
    }

    fn init_wvw_upload_worker(&self) -> Receiver<wvwupload::WvwUploadJob> {
        let (tx, rx) = mpsc::channel();
        *self.wvw_upload_worker.lock().unwrap() = Some(tx);
        rx
    }

    fn init_filewatcher(&self, path: PathBuf) {
        // Maybe instead of the separate receiver, we can just use producer_rx
        let (tx, rx) = std::sync::mpsc::channel();
        // unwrap this, this can only fail, if creating the semaphore fails
        // ReadDirectoryChangesWatcher is really inconsistent on wine, fall back to PollWatcher
        let mut watcher: Box<dyn Watcher + Send> = if winecheck::is_wine() {
            Box::new(
                PollWatcher::new(
                    tx,
                    notify::Config::default().with_poll_interval(Duration::from_secs(5)),
                )
                .unwrap(),
            )
        } else {
            Box::new(RecommendedWatcher::new(tx, notify::Config::default()).unwrap())
        };
        if path.exists() && path.is_dir() {
            // panics if file doesn't exist, but we just checked. I know toctou but this should be fine
            // also can panic on channel errors (very unlikely)
            watcher
                .watch(&path, notify::RecursiveMode::Recursive)
                .unwrap();
        } else {
            log::warn!(
                "Could not watch directory. {} does not exist or is not a directory",
                path.display()
            );
        }
        *self.filewatcher.lock().unwrap() = Some(watcher);
        *self.file_rx.lock().unwrap() = Some(rx);
    }

    fn append_thread(&self, handle: thread::JoinHandle<()>) {
        self.threads.lock().unwrap().push(handle);
    }
    pub fn unwatch(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        if let Some(watcher) = self.filewatcher.lock().unwrap().as_mut()
            && let Err(e) = watcher.unwatch(path)
        {
            log::error!("Failed to unwatch {}: {e}", path.display());
        }
    }
    pub fn watch(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        if let Some(watcher) = self.filewatcher.lock().unwrap().as_mut()
            && let Err(e) = watcher.watch(path, RecursiveMode::Recursive)
        {
            log::error!("Failed to watch {}: {e}", path.display());
        }
    }
}

// This could almost be a thread local but it's required to be safely unloaded which runs on a
// different thread
static STATE: State = State {
    producer_rx: Mutex::new(None),
    evtc_worker: Mutex::new(None),
    filewatcher: Mutex::new(None),
    file_rx: Mutex::new(None),
    dps_worker: Mutex::new(None),
    wingman_worker: Mutex::new(None),
    wvw_upload_worker: Mutex::new(None),
    threads: Mutex::new(Vec::new()),
    logs: Mutex::new(Vec::new()),
};
const KB_IDENTIFIER: &str = "KB_OPEN_WINGMAN_UPLOADS";
const KB_WVW_IDENTIFIER: &str = "KB_OPEN_WVW_SESSIONS";
const QA_WVW_IDENTIFIER: &str = "QA_LOG_UPLOADER_WVW_SESSIONS";

fn collect_urls(logs: &[arcdpslog::Log], settings: &Settings) -> String {
    let mut urls = vec![];
    for l in logs {
        if let Step::Done(ref dpsreport) = l.dpsreport {
            match (
                dpsreport.encounter.success,
                settings.copy_success,
                settings.copy_failure,
            ) {
                (true, false, _) => continue,
                (false, _, false) => continue,
                _ => urls.push(format_url(dpsreport, &settings.dpsreport_copyformat)),
            }
        }
    }
    urls.join("\r\n")
}

fn format_url(dpsreport: &dpsreport::DpsReportResponse, format_template: &str) -> String {
    /*
        @1 - permalink
        @2 - boss name & mode
        @3 - boss id
        @4 - encounter success/fail
    */
    let success = match dpsreport.encounter.success {
        true => "Success",
        false => "Fail",
    };
    let mode = dpsreport
        .encounter
        .format_mode()
        .unwrap_or_else(|| "Unknown".to_string());
    let cm = if mode.is_empty() {
        "".to_string()
    } else {
        format!(" ({})", mode)
    };

    format_template
        .replace("@1", &dpsreport.permalink)
        .replace("@2", &format!("{}{}", dpsreport.encounter.boss, cm))
        .replace("@3", &dpsreport.encounter.boss_id.to_string())
        .replace("@4", success)
}

fn load() {
    log::info!("Loading log-uploader");
    assets::init_textures();
    // lots of locking and relocking but should be fine, since nothing is running
    let producer_tx = STATE.init_producer();
    // Todo move failure handling to from_path impl
    Settings::from_path(settings::config_path()).unwrap_or_else(|e| {
        log::error!("Failed to load settings, using default. Error: {e}");
        Settings::get().init();
    });
    STATE.init_filewatcher(Settings::get().logpath().into());
    let evtc_rx = STATE.init_evtc_worker();
    STATE.append_thread(evtc::run(evtc_rx, producer_tx.clone()));
    let dpsreport_rx = STATE.init_dps_worker();
    STATE.append_thread(dpsreport::run(dpsreport_rx, producer_tx.clone()));
    let wingman_rx = STATE.init_wingman_worker();
    STATE.append_thread(wingman::run(wingman_rx, producer_tx.clone()));
    let wvw_upload_rx = STATE.init_wvw_upload_worker();
    STATE.append_thread(wvwupload::run(wvw_upload_rx, producer_tx.clone()));
    STATE.append_thread(aleeva::run(producer_tx.clone()));
    STATE.append_thread(wvwsession::run(producer_tx.clone()));
    // Verify the stored api key on startup if Aleeva is configured.
    {
        let settings = Settings::get();
        if settings.enable_aleeva && !settings.wvw_only && !settings.aleeva_api_key.is_empty() {
            aleeva::send(aleeva::AleevaCommand::Verify);
        }
    }
    if Settings::get().enable_wvw_sessions {
        wvwsession::send(wvwsession::SessionCommand::FetchProviders);
        if wvwsession::is_configured() {
            wvwsession::send(wvwsession::SessionCommand::Resume);
        }
    }

    register_render(RenderType::Render, render!(render_fn)).revert_on_unload();
    register_render(RenderType::OptionsRender, render!(render_options)).revert_on_unload();
    sync_wvw_registrations(Settings::get().enable_wvw_sessions);
    register_keybind_with_struct(
        KB_IDENTIFIER,
        keybind_handler!(|_, is_release| if !is_release {
            let mut settings = Settings::get();
            settings.show_window = !settings.show_window;
            persist_window_state(&settings);
        }),
        Keybind {
            key: 17,
            alt: true,
            ctrl: false,
            shift: true,
        },
    )
    .revert_on_unload();
    log::info!("Loaded log-uploader");
}
fn unload() {
    log::info!("Unloading log-uploader");
    let settings = Settings::get();
    log::trace!("Unwatching logpath");
    STATE.unwatch(settings.logpath());
    drop(settings);
    log::trace!("Closing channels");
    drop(STATE.producer_rx.lock().unwrap().take());
    drop(STATE.evtc_worker.lock().unwrap().take());
    drop(STATE.filewatcher.lock().unwrap().take());
    drop(STATE.file_rx.lock().unwrap().take());
    drop(STATE.dps_worker.lock().unwrap().take());
    drop(STATE.wingman_worker.lock().unwrap().take());
    drop(STATE.wvw_upload_worker.lock().unwrap().take());
    aleeva::shutdown();
    wvwsession::shutdown();
    // Registered by sync_wvw_registrations, which runs on every toggle and so
    // cannot use revert_on_unload without piling up handles.
    remove_quick_access(QA_WVW_IDENTIFIER);
    unregister_keybind(KB_WVW_IDENTIFIER);

    log::trace!("Waiting on threads");
    for t in STATE.threads.lock().unwrap().drain(..) {
        let threadname = t
            .thread()
            .name()
            .map(String::from)
            .unwrap_or_else(|| format!("{:?}", t.thread().id()));
        log::trace!("Waiting on thread {}", threadname);
        if let Err(e) = t.join() {
            log::error!("Failed to join thread {}: {:#?}", threadname, e);
        }
    }
    // Call this to run destructors (free the vec)
    *STATE.logs.lock().unwrap().as_mut() = vec![];
    log::trace!("Unloaded");
}

fn get_new_logs(logs: &mut Vec<arcdpslog::Log>) {
    let file_rx = STATE.file_rx.lock().unwrap();
    let Some(file_rx) = file_rx.as_ref() else {
        return;
    };
    while let Ok(iter) = file_rx.next_log() {
        for l in iter {
            log::info!("New log found: {}", l.display());
            let log = arcdpslog::Log::new(l);
            EV_LOG_DETECTED.raise(&LogDetectedEvent {
                file_path: log.location_c.as_ptr(),
                file_path_len: log.location_c.as_bytes().len() as u32,
            });
            logs.push(log);
        }
    }
}

fn update_logs(logs: &mut [arcdpslog::Log]) {
    while let Some(WorkerMessage { index, payload }) = STATE.try_next_producer() {
        match payload {
            WorkerType::Evtc(evtc) => {
                if let Ok(ref enc) = evtc {
                    let cpath = &logs[index].location_c;
                    EV_LOG_PARSED.raise(&LogParsedEvent {
                        file_path: cpath.as_ptr(),
                        file_path_len: cpath.as_bytes().len() as u32,
                        boss_id: enc.header.boss_id,
                        player_count: enc.agents.len() as u32,
                    });
                }
                logs[index].evtc = Step::from_value(evtc);
            }
            WorkerType::DpsReport(r) => match r {
                Ok(Ok(r)) => {
                    let mut settings = Settings::get();
                    // If the token changed, we need to update it
                    // Also persist to disk so user doesn't have to press save in options
                    // might freeze the game on first log upload after install
                    if settings.dpsreport_token != r.user_token {
                        settings.dpsreport_token = r.user_token.clone();
                        settings.store(settings::config_path()).unwrap_or_else(|e| {
                            log::error!("Failed to store settings: {e}");
                        });
                    };
                    let cpath = &logs[index].location_c;
                    let cpermalink = CString::new(r.permalink.as_str()).unwrap_or_default();
                    EV_DPSREPORT.raise(&DpsReportEvent {
                        file_path: cpath.as_ptr(),
                        file_path_len: cpath.as_bytes().len() as u32,
                        permalink: cpermalink.as_ptr(),
                        permalink_len: cpermalink.as_bytes().len() as u32,
                        boss_id: r.encounter.boss_id,
                        success: r.encounter.success,
                    });
                    logs[index].dpsreport = Step::from_value(Ok(r));
                }
                Ok(Err(e)) => {
                    logs[index].dpsreport = Step::Retry(e);
                }
                Err(e) => {
                    logs[index].dpsreport = Step::from_value(Err(e));
                }
            },
            WorkerType::Wingman(WingmanUpdate::Progress(p)) => {
                logs[index].wingman_progress = Some(p);
            }
            WorkerType::Wingman(update) => {
                logs[index].wingman_progress = None;
                let (accepted, url) = match &update {
                    WingmanUpdate::Done(url) => (true, url.as_deref().unwrap_or_default()),
                    _ => (false, ""),
                };
                if !matches!(update, WingmanUpdate::Skipped) {
                    let cpath = &logs[index].location_c;
                    let boss_id = match &logs[index].evtc {
                        Step::Done(enc) => enc.header.boss_id,
                        _ => 0,
                    };
                    let curl = CString::new(url).unwrap_or_default();
                    EV_WINGMAN.raise(&WingmanEvent {
                        version: size_of::<WingmanEvent>() as u32,
                        file_path: cpath.as_ptr(),
                        file_path_len: cpath.as_bytes().len() as u32,
                        boss_id,
                        accepted,
                        url: curl.as_ptr(),
                        url_len: curl.as_bytes().len() as u32,
                    });
                }
                logs[index].wingman = match update {
                    WingmanUpdate::Done(url) => Step::Done(url),
                    WingmanUpdate::Skipped => Step::Skipped,
                    WingmanUpdate::Error(e) => Step::Error(e),
                    WingmanUpdate::Progress(_) => unreachable!(),
                };
            }
            WorkerType::Aleeva(r) => {
                logs[index].aleeva = Step::from_value(r);
            }
            WorkerType::Session(r) => {
                // Retryable: the log is already uploaded and has an id, so a
                // failure here is the session service being unreachable.
                logs[index].session = match r {
                    Ok(v) => Step::Done(v),
                    Err(e) => {
                        log::warn!("[Session] {} failed, retrying: {e}", index);
                        Step::Retry(Instant::now() + Duration::from_secs(30))
                    }
                };
            }
            WorkerType::WvwUpload(r) => {
                // Retryable for the same reason dps.report is: the log is still
                // on disk, so another go costs nothing but the upload. A log
                // that never gets an id has no way into the session at all.
                match r {
                    Ok(WvwUploadUpdate::Uploaded(id)) => logs[index].wvw_upload = Step::Done(id),
                    Ok(WvwUploadUpdate::Parse(parse)) => logs[index].wvw_parse = Some(parse),
                    Err(e) => {
                        log::warn!("[WvwUpload] {index} failed, retrying: {e}");
                        logs[index].wvw_upload =
                            Step::Retry(Instant::now() + Duration::from_secs(30));
                    }
                }
            }
        }
    }
}

fn advance_logs(logs: &mut [arcdpslog::Log]) {
    let evtc_tx = STATE.evtc_worker.lock().unwrap();
    let Some(evtc_tx) = evtc_tx.as_ref() else {
        return;
    };
    let dps_tx = STATE.dps_worker.lock().unwrap();
    let Some(dps_tx) = dps_tx.as_ref() else {
        return;
    };
    let wingman_tx = STATE.wingman_worker.lock().unwrap();
    let Some(wingman_tx) = wingman_tx.as_ref() else {
        return;
    };
    let wvw_upload_tx = STATE.wvw_upload_worker.lock().unwrap();
    let Some(wvw_upload_tx) = wvw_upload_tx.as_ref() else {
        return;
    };
    // This can easily be extended to support other stuff like discord webhooks
    for (i, l) in logs.iter_mut().enumerate() {
        if matches!(l.evtc, Step::Pending) {
            log::trace!("Activating evtc job for {}", l.location.display());
            l.evtc = Step::Active;
            if let Err(e) = evtc_tx.send((i, l.location.clone())) {
                log::error!("Failed to send evtc job: {e}");
            }
        }
        // cannot do anything else until the evtc is done
        if matches!(l.evtc, Step::Active) {
            log::trace!("we still parsing evtc for {}", l.location.display());
            continue;
        }
        if let Step::Error(e) = &l.evtc {
            log::error!("Failed to parse evtc for {}: {e}", l.location.display());
            continue;
        }
        if matches!(l.dpsreport, Step::Pending) {
            let settings = Settings::get();
            let token = settings.dpsreport_token.clone();
            let Step::Done(ref enc) = l.evtc else {
                unreachable!()
            };
            // WvW only mode turns dps.report off entirely: a session gets its
            // own parse from evtc.bel.st, so nothing in such a setup has any
            // use for the upload.
            let enabled = settings.enable_dpsreport() && !settings.wvw_only;
            if enabled && !settings.filter_dpsreport.contains(&enc.header.boss_id) {
                l.dpsreport = Step::Active;
                if let Err(e) = dps_tx.send(dpsreport::DpsJob {
                    index: i,
                    location: l.location.clone(),
                    token,
                }) {
                    log::error!("Failed to send dpsreport job: {e}");
                }
            } else {
                l.dpsreport = Step::Skipped;
            }
        }
        if matches!(l.wingman, Step::Pending) {
            let settings = Settings::get();
            let enabled = settings.enable_wingman && !settings.wvw_only;
            let Step::Done(ref enc) = l.evtc else {
                unreachable!()
            };
            if enabled
                && enc.header.boss_id != WVW_BOSS_ID
                && !settings.filter_wingman.contains(&enc.header.boss_id)
            {
                // I wonder if I can do this without the if check since this is guaranteed to be
                // done
                l.wingman = Step::Active;
                if let Err(e) = wingman_tx.send((
                    i,
                    l.location.clone(),
                    // Error handling on missing pov (broken log?)
                    enc.pov.clone().map(|a| a.account_name).unwrap_or_default(),
                    enc.header.boss_id,
                )) {
                    log::error!("Failed to send wingman job: {e}");
                }
            } else {
                l.wingman = Step::Skipped;
            }
        }
        // Aleeva depends on the dps.report permalink, so it only runs once
        // dps.report is done. If dps.report is skipped/failed there is no
        // permalink to post, so Aleeva is skipped too.
        if matches!(l.aleeva, Step::Pending) {
            match &l.dpsreport {
                Step::Done(dps) => {
                    let s = Settings::get();
                    if s.enable_aleeva && !s.wvw_only && aleeva::is_authorised() {
                        // Build the set of (normalised) account names present
                        // in this log so we can match groups efficiently.
                        let log_players: HashSet<String> = {
                            // EVTC is guaranteed Done here (see early-returns above).
                            let Step::Done(enc) = &l.evtc else {
                                unreachable!()
                            };
                            enc.agents
                                .iter()
                                .filter(|a| !a.account_name.is_empty())
                                .map(|a| {
                                    a.account_name.trim_start_matches(':').to_ascii_lowercase()
                                })
                                .collect()
                        };

                        let mut targets: Vec<aleeva::AleevaPostTarget> = Vec::new();
                        let mut any_group_matched = false;

                        // Check each configured group.
                        for group in &s.aleeva_groups {
                            let match_count = group
                                .players
                                .iter()
                                .filter(|p| log_players.contains(p.to_ascii_lowercase().as_str()))
                                .count();
                            if group.min_players > 0 && match_count >= group.min_players {
                                any_group_matched = true;
                                if !group.target.server_id.is_empty() {
                                    targets.push(aleeva::AleevaPostTarget {
                                        server_id: group.target.server_id.clone(),
                                        channel_id: group.target.channel_id.clone(),
                                        send_notification: group.target.send_notification,
                                    });
                                }
                            }
                        }

                        // Add the default target when applicable.
                        let add_default = if s.aleeva_default_posts_unmatched_only {
                            !any_group_matched
                        } else {
                            true
                        };
                        if add_default && !s.aleeva_selected_server_id.is_empty() {
                            targets.push(aleeva::AleevaPostTarget {
                                server_id: s.aleeva_selected_server_id.clone(),
                                channel_id: s.aleeva_selected_channel_id.clone(),
                                send_notification: s.aleeva_send_notification,
                            });
                        }

                        if !targets.is_empty() {
                            l.aleeva = Step::Active;
                            aleeva::send(aleeva::AleevaCommand::Post(aleeva::AleevaJob {
                                index: i,
                                permalink: dps.permalink.clone(),
                                targets,
                            }));
                        } else {
                            l.aleeva = Step::Skipped;
                        }
                    } else {
                        l.aleeva = Step::Skipped;
                    }
                }
                Step::Skipped | Step::Error(_) => {
                    l.aleeva = Step::Skipped;
                }
                // dps.report still pending/active/retry: wait for it.
                _ => {}
            }
        }
        if matches!(l.wvw_upload, Step::Pending) {
            let Step::Done(ref enc) = l.evtc else {
                unreachable!()
            };
            if enc.header.boss_id == WVW_BOSS_ID
                && wvwsession::is_configured()
                && wvwsession::is_active()
            {
                l.wvw_upload = Step::Active;
                if let Err(e) = wvw_upload_tx.send(wvwupload::WvwUploadJob {
                    index: i,
                    location: l.location.clone(),
                    account: enc.pov.clone().map(|a| a.account_name).unwrap_or_default(),
                    token: Settings::get().wvw_token.clone(),
                }) {
                    log::error!("Failed to send wvw upload job: {e}");
                    l.wvw_upload = Step::Skipped;
                }
            } else {
                l.wvw_upload = Step::Skipped;
            }
        }
        // The session registers the id the upload came back with.
        if matches!(l.session, Step::Pending) {
            match &l.wvw_upload {
                Step::Done(log_id) => {
                    // The slug is captured here so a retry cannot be
                    // redirected into a session started in the meantime.
                    if let Some(slug) = wvwsession::active_slug() {
                        l.session = Step::Active;
                        wvwsession::send(wvwsession::SessionCommand::AddLog {
                            index: i,
                            log_id: log_id.clone(),
                            slug,
                        });
                    } else {
                        l.session = Step::Skipped;
                    }
                }
                Step::Skipped | Step::Error(_) => {
                    l.session = Step::Skipped;
                }
                // Still uploading or waiting to retry.
                _ => {}
            }
        }
        if let Step::Retry(t) = l.wvw_upload {
            if l.wvw_upload_count > 3 {
                l.wvw_upload = Step::Error(anyhow::anyhow!("Retry limit reached"));
            } else if Instant::now() > t {
                l.wvw_upload_count += 1;
                l.wvw_upload = Step::Pending;
                // The next attempt gets its own id and ticket, so whatever the
                // last one's parse was doing does not describe this log.
                l.wvw_parse = None;
            }
        }
        if let Step::Retry(t) = l.dpsreport {
            if l.dpsreport_count > 3 {
                l.dpsreport = Step::Error(anyhow::anyhow!("Retry limit reached"));
            } else if Instant::now() > t {
                l.dpsreport_count += 1;
                l.dpsreport = Step::Pending;
            }
        }
        if let Step::Retry(t) = l.session {
            if l.session_count > 3 {
                l.session = Step::Error(anyhow::anyhow!("Retry limit reached"));
            } else if Instant::now() > t {
                l.session_count += 1;
                l.session = Step::Pending;
            }
        }
    }
}

fn setup_table<F: FnOnce()>(ui: &Ui, f: F) {
    let flags =
        TableFlags::BORDERS_OUTER | TableFlags::BORDERS_INNER_V | TableFlags::NO_PAD_INNER_X;

    let max_time_width = ui.calc_text_size("00:00 (Thu Nov 14)")[0];
    let max_path_width = ui.calc_text_size("Kanaxai, Scythe of House Aurkus (25577)")[0];

    let t = ui.begin_table_header_with_flags(
        e("Uploads"),
        [
            TableColumnSetup {
                name: e("Encounter"),
                flags: TableColumnFlags::WIDTH_STRETCH,
                init_width_or_weight: max_path_width + 10.0,
                user_id: Default::default(),
            },
            TableColumnSetup {
                name: e("Created"),
                flags: TableColumnFlags::WIDTH_FIXED,
                init_width_or_weight: max_time_width + 10.0,
                user_id: Default::default(),
            },
            TableColumnSetup {
                // DpsReport
                name: e("##dpsreportheader"),
                flags: TableColumnFlags::WIDTH_FIXED,
                init_width_or_weight: 20.0,
                user_id: Default::default(),
            },
            TableColumnSetup {
                // Aleeva
                name: e("##aleevaheader"),
                flags: TableColumnFlags::WIDTH_FIXED,
                init_width_or_weight: 20.0,
                user_id: Default::default(),
            },
            TableColumnSetup {
                // Wingman
                name: e("##wingmanheader"),
                flags: TableColumnFlags::WIDTH_FIXED,
                init_width_or_weight: 20.0,
                user_id: Default::default(),
            },
            TableColumnSetup {
                // WvW session upload
                name: e("##wvwuploadheader"),
                flags: TableColumnFlags::WIDTH_FIXED,
                init_width_or_weight: 20.0,
                user_id: Default::default(),
            },
            TableColumnSetup {
                // Open in Folder
                name: e("##openinfolderheader"),
                flags: TableColumnFlags::WIDTH_FIXED,
                init_width_or_weight: 20.0,
                user_id: Default::default(),
            },
        ],
        flags,
    );
    if let Some(t) = t {
        f();
        t.end();
    }
}

// Notification window for misspelled logpath (hotfix 20241114)
fn render_hotfix20241114(ui: &Ui, settings: &mut Settings) {
    if settings.check_hotfix20241114()
        && !settings.hide_hotfix_notification_20241114
        && let Some(_w) = Window::new(e("Arcdps Path Fix"))
            .collapsible(false)
            .begin(ui)
    {
        ui.text(format!(
            r#"Unfortunately there was a typo in the default logpath last version.
You seem to have the wrong logpath ({}) configured.
Do you want to set it to the default logpath ({})?
You can also hide this message permanently if the configured path is correct."#,
            settings.logpath,
            Settings::default_dir().display()
        ));
        if ui.button(e("Reset logpath to default")) {
            STATE.unwatch(&settings.logpath);
            settings.fix_hotfix20241114();
            STATE.watch(&settings.logpath);
            _ = settings.store(settings::config_path());
        }
        if ui.button(e("Don't show this window again")) {
            settings.hide_hotfix_notification_20241114 = true;
            _ = settings.store(settings::config_path());
        }
    }
}

fn persist_window_state(settings: &Settings) {
    if let Err(e) = settings.store_window_state(settings::config_path()) {
        log::error!("Failed to store window state: {e}");
    }
}

fn render_fn(ui: &Ui) {
    FRAME_NUM.set(FRAME_NUM.get() + 1);
    let mut logs = STATE.logs.lock().unwrap();
    get_new_logs(&mut logs);
    update_logs(&mut logs);
    advance_logs(&mut logs);

    let mut settings = Settings::get();
    render_hotfix20241114(ui, &mut settings);
    let was_open = settings.show_window;
    if settings.show_window
        && let Some(_w) = Window::new(e("Log Uploader"))
            .opened(&mut settings.show_window)
            .collapsible(false)
            .begin(ui)
    {
        ChildWindow::new("Log Table")
            .size([0.0, -ui.frame_height_with_spacing() * 2.0])
            .always_auto_resize(true)
            .build(ui, || {
                if logs.is_empty() {
                    ui.text(e("No logs yet."));
                    return;
                }
                setup_table(ui, || {
                    if settings.rev_log_order {
                        for l in logs.iter().rev() {
                            l.render_row(ui);
                        }
                    } else {
                        for l in logs.iter() {
                            l.render_row(ui);
                        }
                    }
                });
            });

        let controls = ui.begin_group();
        ui.align_text_to_frame_padding();
        ui.text(e("Include:"));
        ui.same_line();
        ui.checkbox("Success", &mut settings.copy_success);
        ui.same_line();
        ui.checkbox("Failure", &mut settings.copy_failure);

        if ui.button(e("Copy dps.report urls")) {
            let urls = collect_urls(&logs, &settings);
            if !urls.is_empty() {
                ui.set_clipboard_text(urls);
            }
        }
        controls.end();
    }
    let wvw_was_open = settings.show_wvw_window;
    render_wvw_window(ui, &mut settings);
    render_wvw_indicator(ui, &settings);
    sync_wvw_quick_access_icon(&settings);
    if was_open != settings.show_window || wvw_was_open != settings.show_wvw_window {
        persist_window_state(&settings);
    }

    settings.render_reminder(ui);
}

thread_local! {
    static SESSION_NAME: RefCell<String> = const { RefCell::new(String::new()) };
}

const DEFAULT_INDICATOR_POS: [f32; 2] = [20.0, 20.0];

fn color_amber(ui: &Ui) -> [f32; 4] {
    ui.clone_style()[StyleColor::PlotHistogram]
}

fn color_disabled(ui: &Ui) -> [f32; 4] {
    ui.clone_style()[StyleColor::TextDisabled]
}

fn format_elapsed(elapsed: chrono::TimeDelta) -> String {
    let secs = elapsed.num_seconds().max(0);
    format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
}

fn state_dot(ui: &Ui, color: [f32; 4], pulsing: bool) {
    let size = ui.text_line_height();
    let radius = size * 0.22;
    let [x, y] = ui.cursor_screen_pos();
    let alpha = if pulsing {
        // Slow breath so it reads as live without pulling the eye off the game.
        0.45 + 0.55 * (0.5 + 0.5 * (ui.time() as f32 * 2.0).sin())
    } else {
        1.0
    };
    let color = [color[0], color[1], color[2], color[3] * alpha];
    ui.get_window_draw_list()
        .add_circle([x + radius, y + size * 0.5], radius, color)
        .filled(true)
        .num_segments(16)
        .build();
    ui.dummy([radius * 2.0, size]);
    ui.same_line();
}

fn render_wvw_indicator(ui: &Ui, settings: &Settings) {
    if !settings.enable_wvw_sessions || !settings.wvw_indicator {
        return;
    }
    let session = wvwsession::active();
    let combining = session.is_none() && wvwsession::is_combining();
    if session.is_none() && !combining {
        return;
    }

    Window::new("##wvwsessionindicator")
        .position(DEFAULT_INDICATOR_POS, Condition::FirstUseEver)
        .title_bar(false)
        .resizable(false)
        .scroll_bar(false)
        .scrollable(false)
        .collapsible(false)
        .always_auto_resize(true)
        .focus_on_appearing(false)
        .build(ui, || {
            let Some(session) = session else {
                state_dot(ui, color_amber(ui), false);
                ui.text_disabled(e("Combining"));
                if ui.is_window_hovered() {
                    ui.tooltip_text(e("The report is being built."));
                }
                return;
            };

            state_dot(ui, common::RED, true);
            // Reserve the widest reading once so the marker holds its width
            // instead of twitching every time a digit rolls over.
            let elapsed = format_elapsed(session.elapsed());
            let width = ui.calc_text_size("0:00:00")[0].max(ui.calc_text_size(&elapsed)[0]);
            let start_x = ui.cursor_pos()[0];
            ui.text(&elapsed);
            ui.same_line_with_pos(start_x + width);

            let (logs, sent) = (session.logs(), session.sent());
            ui.text_disabled("|");
            ui.same_line();
            if session.has_failures() {
                ui.text_colored(common::RED, format!("{sent}/{logs}"));
                ui.same_line();
                ui.text_disabled(e("sent"));
            } else {
                ui.text(logs.to_string());
                ui.same_line();
                ui.text_disabled(e("logs"));
            }

            if let Some(name) = &session.name {
                ui.same_line();
                ui.text_disabled("|");
                ui.same_line();
                ui.text(name);
            }

            if ui.is_window_hovered() {
                ui.tooltip_text(e("A WvW session is recording."));
            }
        });
}

fn render_wvw_window(ui: &Ui, settings: &mut Settings) {
    if !settings.enable_wvw_sessions || !settings.show_wvw_window {
        return;
    }
    let min_width = ui.calc_text_size(e("Building the report. Usually a minute or two."))[0];
    let Some(_w) = Window::new(e("WvW Sessions"))
        .opened(&mut settings.show_wvw_window)
        .always_auto_resize(true)
        .size_constraints([min_width, 0.0], [f32::MAX, f32::MAX])
        .begin(ui)
    else {
        return;
    };

    if settings.wvw_token.is_empty() {
        ui.text_disabled(e("Not logged in."));
        ui.text_disabled(e("Log in from the addon options to start a session."));
        return;
    }

    let state = wvwsession::snapshot();
    render_wvw_status(ui, &state);
    ui.separator();

    if state.busy {
        ui.text(e("Working..."));
    } else {
        match &state.current {
            Some(session) => render_wvw_recording(ui, session),
            None => render_wvw_idle(ui, &state),
        }
    }

    if let Some(error) = &state.last_error {
        ui.spacing();
        ui.text_colored(common::RED, error);
    }

    ui.separator();
    render_wvw_history(ui, &state);
}

// Account on the left, what the session is doing on the right.
fn render_wvw_status(ui: &Ui, state: &wvwsession::SessionState) {
    let (color, label, pulsing) = match (&state.current, wvwsession::is_combining()) {
        (Some(session), _) if session.has_failures() => (common::RED, e("Uploads failing"), true),
        (Some(_), _) => (common::RED, e("Recording"), true),
        (None, true) => (color_amber(ui), e("Combining"), false),
        (None, false) => (color_disabled(ui), e("Idle"), false),
    };
    state_dot(ui, color, pulsing);
    match &state.account {
        Some(account) => ui.text(account),
        None => ui.text_disabled(e("Checking login...")),
    }

    let label_width = ui.calc_text_size(&label)[0];
    ui.same_line_with_pos(ui.content_region_max()[0] - label_width);
    ui.text_colored(color, label);
}

fn render_wvw_recording(ui: &Ui, session: &wvwsession::ActiveSession) {
    if let Some(name) = &session.name {
        ui.text(name);
    } else {
        ui.text_disabled(e("Unnamed session"));
    }

    let (logs, sent) = (session.logs(), session.sent());
    let sent_color = if session.has_failures() {
        common::RED
    } else {
        common::GREEN
    };
    if let Some(_t) = ui.begin_table_with_flags("##sessionstats", 3, TableFlags::BORDERS) {
        ui.table_next_row();
        stat_cell(ui, &format_elapsed(session.elapsed()), e("Elapsed"), None);
        stat_cell(ui, &logs.to_string(), e("Logs"), None);
        stat_cell(ui, &sent.to_string(), e("Sent"), Some(sent_color));
    }

    match (&session.last_error, session.last_upload) {
        (Some(error), _) => {
            ui.text_colored(common::RED, e("Last upload failed"));
            ui.text_wrapped(error);
            ui.text_disabled(e("Retrying automatically."));
        }
        (None, Some(at)) => ui.text_disabled(format!(
            "{} {}s {}",
            e("Last upload"),
            at.elapsed().as_secs(),
            e("ago")
        )),
        (None, None) => ui.text_disabled(e("No logs uploaded yet.")),
    }

    if ui.button_with_size(e("Stop & combine"), [-1.0, 0.0]) {
        wvwsession::send(wvwsession::SessionCommand::Stop);
    }
    if ui.is_item_hovered() {
        ui.tooltip_text(e(
            "Stops recording and combines the collected logs into one report.",
        ));
    }
}

fn render_wvw_idle(ui: &Ui, state: &wvwsession::SessionState) {
    if let Some(session) = state
        .history
        .iter()
        .find(|s| s.state == wvwsession::SessionLifecycle::Combining)
    {
        ui.text(session.label());
        ProgressBarIndeterminate::new()
            .size([-1.0, ui.text_line_height() * 0.4])
            .overlay_text(" ")
            .build(ui);
        ui.text_disabled(e(
            "Building the report. Usually a minute or two, and you can close this window.",
        ));
        ui.separator();
    }

    SESSION_NAME.with_borrow_mut(|name| {
        ui.set_next_item_width(-1.0);
        if ui
            .input_text("##sessionname", name)
            .hint(e("Name this session (optional)"))
            .enter_returns_true(true)
            .build()
        {
            start_session(name);
        }
    });
    if ui.button_with_size(e("Start session"), [-1.0, 0.0]) {
        SESSION_NAME.with_borrow_mut(start_session);
    }
    if ui.is_item_hovered() {
        ui.tooltip_text(e(
            "Collects the WvW logs recorded from now on and combines\n\
             them into a single report when you stop.",
        ));
    }
}

fn start_session(name: &mut String) {
    let trimmed = name.trim();
    let name_arg = (!trimmed.is_empty()).then(|| trimmed.to_owned());
    wvwsession::send(wvwsession::SessionCommand::Start { name: name_arg });
    name.clear();
}

/// One figure of the recording stat row: number over its label.
fn stat_cell(ui: &Ui, value: &str, label: String, color: Option<[f32; 4]>) {
    ui.table_next_column();
    match color {
        Some(color) => ui.text_colored(color, value),
        None => ui.text(value),
    }
    ui.text_disabled(label);
}

/// Sessions still combining are listed too, without a button.
fn render_wvw_history(ui: &Ui, state: &wvwsession::SessionState) {
    ui.text_disabled(e("Past sessions"));
    ui.same_line();
    if ui.small_button(e("Refresh")) {
        wvwsession::send(wvwsession::SessionCommand::FetchHistory);
    }

    if state.history.is_empty() {
        ui.text_disabled(if state.history_loaded {
            e("No sessions yet.")
        } else {
            e("Loading...")
        });
        return;
    }

    ChildWindow::new("WvW Session History")
        .size([0.0, (ui.frame_height_with_spacing() * 6.0).min(200.0)])
        .build(ui, || {
            let Some(_t) = ui.begin_table_with_flags(
                "##sessionhistory",
                3,
                TableFlags::ROW_BG | TableFlags::SIZING_FIXED_FIT,
            ) else {
                return;
            };
            ui.table_setup_column_with(TableColumnSetup {
                name: "##state",
                flags: TableColumnFlags::WIDTH_FIXED,
                init_width_or_weight: ui.calc_text_size(e("Combining"))[0],
                user_id: Default::default(),
            });
            ui.table_setup_column_with(TableColumnSetup {
                name: "##label",
                flags: TableColumnFlags::WIDTH_STRETCH,
                init_width_or_weight: 0.0,
                user_id: Default::default(),
            });
            ui.table_setup_column("##action");

            for session in &state.history {
                let _id = ui.push_id(&session.slug);
                ui.table_next_row();
                ui.table_next_column();
                match session.state {
                    wvwsession::SessionLifecycle::Ready => {
                        ui.text_colored(common::GREEN, e("Ready"))
                    }
                    wvwsession::SessionLifecycle::Combining => {
                        ui.text_colored(color_amber(ui), e("Combining"))
                    }
                    wvwsession::SessionLifecycle::Failed => {
                        ui.text_colored(common::RED, e("Failed"))
                    }
                    wvwsession::SessionLifecycle::Open | wvwsession::SessionLifecycle::Unknown => {
                        ui.text_disabled(e("Unknown"))
                    }
                }

                ui.table_next_column();
                ui.text(session.label());

                ui.table_next_column();
                if session.state == wvwsession::SessionLifecycle::Ready {
                    if ui.small_button(e("Open")) {
                        open_report(&session.slug);
                    }
                } else if session.state == wvwsession::SessionLifecycle::Failed
                    && let Some(error) = &session.error
                {
                    ui.text_disabled("(?)");
                    if ui.is_item_hovered() {
                        ui.tooltip_text(error);
                    }
                }
            }
        });
}

fn open_report(slug: &str) {
    let url = format!("{}/s/{slug}", wvwsession::BASE);
    if let Err(e) = open::that_detached(&url) {
        log::error!("Failed to open {url}: {e}");
    }
}

fn sync_wvw_registrations(enabled: bool) {
    if enabled {
        let _ = register_keybind_with_struct(
            KB_WVW_IDENTIFIER,
            keybind_handler!(|_, is_release| if !is_release {
                let mut settings = Settings::get();
                settings.show_wvw_window = !settings.show_wvw_window;
                persist_window_state(&settings);
            }),
            // Unbound by default, let the user pick.
            Keybind {
                key: 0,
                alt: false,
                ctrl: false,
                shift: false,
            },
        );
        add_wvw_quick_access(wvwsession::is_active());
    } else {
        remove_quick_access(QA_WVW_IDENTIFIER);
        unregister_keybind(KB_WVW_IDENTIFIER);
        QA_SHOWS_ACTIVE.set(None);
    }
}

thread_local! {
    // Which icon the quick access carries, so it is only re-registered when the
    // session state actually changes.
    static QA_SHOWS_ACTIVE: Cell<Option<bool>> = const { Cell::new(None) };
}

fn add_wvw_quick_access(active: bool) {
    let (icon, hover) = if active {
        (assets::WVWSESSION_ACTIVE, assets::WVWSESSION_ACTIVE_HOVER)
    } else {
        (assets::WVWSESSION, assets::WVWSESSION_HOVER)
    };
    let _ = add_quick_access(
        QA_WVW_IDENTIFIER,
        icon,
        hover,
        KB_WVW_IDENTIFIER,
        e("WvW Sessions"),
    );
    QA_SHOWS_ACTIVE.set(Some(active));
}

/// Nexus has no way to change an icon in place, so the marker only appears by
/// registering the entry again with the other texture.
fn sync_wvw_quick_access_icon(settings: &Settings) {
    if !settings.enable_wvw_sessions {
        return;
    }
    let active = wvwsession::is_active();
    if QA_SHOWS_ACTIVE.get() == Some(active) {
        return;
    }
    remove_quick_access(QA_WVW_IDENTIFIER);
    add_wvw_quick_access(active);
}

fn render_options(ui: &Ui) {
    let old = Settings::get().clone();
    settings::render(ui);
    let new = Settings::get().clone();
    if old.logpath != new.logpath {
        STATE.unwatch(old.logpath);
        STATE.watch(new.logpath);
    }
    if old.enable_wvw_sessions != new.enable_wvw_sessions {
        sync_wvw_registrations(new.enable_wvw_sessions);
        if new.enable_wvw_sessions {
            wvwsession::send(wvwsession::SessionCommand::FetchProviders);
        }
    }
}

nexus::export! {
    name: "Log Uploader",
    signature: -69421,
    flags: AddonFlags::None,
    load,
    unload,
    provider: UpdateProvider::GitHub,
    update_link: "https://github.com/belst/nexus-wingman-uploader",
    log_filter: "warn,log_uploader=debug"
}
