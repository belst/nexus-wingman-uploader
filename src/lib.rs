use std::{
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
    imgui::{ChildWindow, TableColumnFlags, TableColumnSetup, TableFlags, Ui, Window},
    keybind::{Keybind, register_keybind_with_struct},
    keybind_handler,
    paths::get_addon_dir,
    render,
};
use notify::{Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use settings::Settings;
use util::e;

mod arcdpslog;
mod assets;
mod common;
mod dpsreport;
mod evtc;
mod filewatcher;
mod settings;
mod util;
mod wingman;

// TODO: grep for all the `let _ =` and add error handling
// TODO: Implement actual dpsreport
// TODO: Icons
struct State {
    producer_rx: Mutex<Option<Receiver<common::WorkerMessage>>>,
    evtc_worker: Mutex<Option<Sender<evtc::EvtcJob>>>,
    filewatcher: Mutex<Option<Box<dyn Watcher + Send>>>,
    file_rx: Mutex<Option<Receiver<Result<Event, notify::Error>>>>,
    dps_worker: Mutex<Option<Sender<dpsreport::DpsJob>>>,
    wingman_worker: Mutex<Option<Sender<wingman::WingmanJob>>>,
    threads: Mutex<Vec<thread::JoinHandle<()>>>,
    logs: Mutex<Vec<arcdpslog::Log>>,
}

impl State {
    /// Returns the receiver for updates from the workers
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
        if let Some(watcher) = self.filewatcher.lock().unwrap().as_mut() {
            if let Err(e) = watcher.unwatch(path) {
                log::error!("Failed to unwatch {}: {e}", path.display());
            }
        }
    }
    pub fn watch(&self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        if let Some(watcher) = self.filewatcher.lock().unwrap().as_mut() {
            if let Err(e) = watcher.watch(path, RecursiveMode::Recursive) {
                log::error!("Failed to watch {}: {e}", path.display());
            }
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
    threads: Mutex::new(Vec::new()),
    logs: Mutex::new(Vec::new()),
};
const KB_IDENTIFIER: &str = "KB_OPEN_WINGMAN_UPLOADS";

fn config_path() -> PathBuf {
    get_addon_dir("wingman-uploader")
        .expect("Addon dir to exist")
        .join("settings.json")
}

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
    let cm = if mode == "" {
        "".to_string()
    } else {
        format!(" ({})", mode)
    };

    return format_template
        .replace("@1", &dpsreport.permalink)
        .replace("@2", &format!("{}{}", dpsreport.encounter.boss, cm))
        .replace("@3", &dpsreport.encounter.boss_id.to_string())
        .replace("@4", &success);
}

fn load() {
    log::info!("Loading log-uploader");
    assets::init_textures();
    // lots of locking and relocking but should be fine, since nothing is running
    let producer_tx = STATE.init_producer();
    Settings::from_path(config_path()).expect("Failed to load settings");
    STATE.init_filewatcher(Settings::get().logpath().into());
    let evtc_rx = STATE.init_evtc_worker();
    STATE.append_thread(evtc::run(evtc_rx, producer_tx.clone()));
    let dpsreport_rx = STATE.init_dps_worker();
    STATE.append_thread(dpsreport::run(dpsreport_rx, producer_tx.clone()));
    let wingman_rx = STATE.init_wingman_worker();
    STATE.append_thread(wingman::run(wingman_rx, producer_tx.clone()));

    register_render(RenderType::Render, render!(render_fn)).revert_on_unload();
    register_render(RenderType::OptionsRender, render!(render_options)).revert_on_unload();
    register_keybind_with_struct(
        KB_IDENTIFIER,
        keybind_handler!(|_, is_release| if !is_release {
            let mut settings = Settings::get_mut();
            settings.show_window = !settings.show_window;
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
    log::trace!("Storing config");
    if let Err(e) = settings.store(config_path()) {
        log::error!("Failed to store settings: {e}");
    }
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
    std::mem::swap(STATE.logs.lock().unwrap().as_mut(), &mut vec![]);
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
            logs.push(arcdpslog::Log::new(l));
        }
    }
}

fn update_logs(logs: &mut [arcdpslog::Log]) {
    while let Some(WorkerMessage { index, payload }) = STATE.try_next_producer() {
        match payload {
            WorkerType::Evtc(evtc) => {
                logs[index].evtc = Step::from_value(evtc);
            }
            WorkerType::DpsReport(r) => match r {
                Ok(Ok(r)) => {
                    Settings::get_mut().dpsreport_token = r.user_token.clone();
                    logs[index].dpsreport = Step::from_value(Ok(r));
                }
                Ok(Err(e)) => {
                    logs[index].dpsreport = Step::Retry(e);
                }
                Err(e) => {
                    logs[index].dpsreport = Step::from_value(Err(e));
                }
            },
            WorkerType::Wingman(r) => {
                logs[index].wingman = Step::from_value(r);
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
            let enabled = settings.enable_dpsreport();
            let token = settings.dpsreport_token.clone();
            let Step::Done(ref enc) = l.evtc else {
                unreachable!()
            };
            if enabled && !settings.filter_dpsreport.contains(&enc.header.boss_id) {
                l.dpsreport = Step::Active;
                if let Err(e) = dps_tx.send((i, l.location.clone(), token)) {
                    log::error!("Failed to send dpsreport job: {e}");
                }
            } else {
                l.dpsreport = Step::Skipped;
            }
        }
        if matches!(l.wingman, Step::Pending) {
            let settings = Settings::get();
            let enabled = settings.enable_wingman;
            let Step::Done(ref enc) = l.evtc else {
                unreachable!()
            };
            if enabled
                && enc.header.boss_id != 1
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
        if let Step::Retry(t) = l.dpsreport {
            if l.dpsreport_count > 3 {
                l.dpsreport = Step::Error(anyhow::anyhow!("Retry limit reached"));
            } else if Instant::now() > t {
                l.dpsreport_count += 1;
                l.dpsreport = Step::Pending;
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
                name: e(""),
                flags: TableColumnFlags::WIDTH_FIXED,
                init_width_or_weight: 20.0,
                user_id: Default::default(),
            },
            TableColumnSetup {
                // Wingman
                name: e(""),
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
    if settings.check_hotfix20241114() && !settings.hide_hotfix_notification_20241114 {
        if let Some(_w) = Window::new(e("Arcdps Path Fix"))
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
            }
            if ui.button(e("Don't show this window again")) {
                settings.hide_hotfix_notification_20241114 = true;
            }
        }
    }
}

fn render_fn(ui: &Ui) {
    let mut logs = STATE.logs.lock().unwrap();
    get_new_logs(&mut logs);
    advance_logs(&mut logs);
    update_logs(&mut logs);

    let mut settings = Settings::get_mut();
    render_hotfix20241114(ui, &mut settings);
    if settings.show_window {
        if let Some(_w) = Window::new(e("Log Uploader"))
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
    }
}

fn render_options(ui: &Ui) {
    let old = Settings::get().clone();
    settings::render(ui);
    let new = Settings::get().clone();
    if old.logpath != new.logpath {
        STATE.unwatch(old.logpath);
        STATE.watch(new.logpath);
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
