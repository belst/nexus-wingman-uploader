use std::{
    path::{Path, PathBuf},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, OnceLock, RwLock,
    },
    thread::JoinHandle,
    time::Duration,
};

use dpslog::{Upload, UploadRef, UploadStatus};
use nexus::{
    gui::{register_render, RenderType},
    imgui::{TableColumnFlags, TableColumnSetup, TableFlags, Ui, Window},
    keybind::{register_keybind_with_struct, Keybind},
    keybind_handler,
    paths::get_addon_dir,
    render,
    texture::load_texture_from_memory,
    AddonFlags, UpdateProvider,
};

use dpsreportupload::DpsReportUploader;
use notify::{
    event::{ModifyKind, RenameMode},
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use settings::Settings;
use ureq::AgentBuilder;
use wingmanupload::WingmanUploader;

mod dpslog;
mod dpsreportupload;
mod settings;
mod ui;
mod wingmanupload;

pub fn agent() -> ureq::Agent {
    let mut builder = AgentBuilder::new()
        .timeout_read(Duration::from_secs(60 * 15))
        .timeout_write(Duration::from_secs(5));
    if let Ok(tls) = native_tls::TlsConnector::new() {
        builder = builder.tls_connector(Arc::new(tls));
    }
    builder.build()
}
// TODO: Less mutable statics everywhere. remove a lot of unsafe
// Everything mut so we can .take() on unload to clean up
static mut THREADS: OnceLock<Vec<JoinHandle<()>>> = OnceLock::new();
static mut FILEPATH_TX: OnceLock<Sender<UploadRef>> = OnceLock::new();
static mut DPSURL_TX: OnceLock<Sender<UploadRef>> = OnceLock::new();
static mut WATCH_EVENTS_RX: OnceLock<Mutex<Receiver<notify::Result<Event>>>> = OnceLock::new();
static mut WATCHER: OnceLock<RwLock<RecommendedWatcher>> = OnceLock::new();
static mut DPS_REPORT_HANDLER: OnceLock<Arc<DpsReportUploader>> = OnceLock::new();

static WINGMAN_LOGO_BYTES: &'static [u8] = include_bytes!("../wingman.png");
static DPSREPORT_LOGO_BYTES: &'static [u8] = include_bytes!("../dpsreport.png");

unsafe fn config_path() -> PathBuf {
    get_addon_dir("wingman-uploader")
        .expect("addon dir to exist")
        .join("settings.json")
}

fn load() {
    load_texture_from_memory("WINGMAN_LOGO", WINGMAN_LOGO_BYTES, None);
    load_texture_from_memory("DPSREPORT_LOGO", DPSREPORT_LOGO_BYTES, None);
    unsafe {
        log::info!("Loading wingman");
        let _ = dpslog::UPLOADS.set(Vec::new());
        let _ = THREADS.set(Vec::new());

        if let Ok(s) = Settings::from_path(config_path()) {
            *Settings::get_mut() = s;
        }
        let token = Settings::get().dpsreport_token.read().unwrap().clone();
        let dpsreport = if !token.is_empty() {
            DpsReportUploader::with_token(token)
        } else {
            DpsReportUploader::new()
        };
        let (filepath_tx, filepath_rx) = mpsc::channel();
        THREADS
            .get_mut()
            .unwrap()
            .push(dpsreport.clone().run(filepath_rx));
        let _ = DPS_REPORT_HANDLER.set(dpsreport);
        let (dpsurl_tx, dpsurl_rx) = mpsc::channel();
        let wingman = WingmanUploader::new();
        THREADS.get_mut().unwrap().push(wingman.run(dpsurl_rx));
        let _ = FILEPATH_TX.set(filepath_tx);
        let _ = DPSURL_TX.set(dpsurl_tx);

        let (tx, rx) = mpsc::channel();
        WATCH_EVENTS_RX.get_or_init(|| Mutex::new(rx));

        let mut watcher = RecommendedWatcher::new(tx, Config::default()).unwrap();
        let arclogspath = Settings::get().logpath.clone();
        set_watch_path(&mut watcher, arclogspath);
        let _ = WATCHER.set(RwLock::new(watcher));

        register_render(RenderType::Render, render!(render_fn)).revert_on_unload();
        register_render(RenderType::OptionsRender, render!(render_options)).revert_on_unload();
        register_keybind_with_struct(
            KB_IDENTIFIER,
            keybind_handler!(keypress),
            Keybind {
                key: 17, // W
                alt: true,
                ctrl: false,
                shift: true,
            },
        )
        .revert_on_unload();
    }
}

const KB_IDENTIFIER: &'static str = "KB_OPEN_WINGMAN_UPLOADS";

fn keypress(_: &str) {
    let settings = Settings::get_mut();
    settings.show_window = !settings.show_window;
}

fn set_watch_path<W: Watcher, P: AsRef<Path>>(w: &mut W, path: P) {
    if let Err(e) = w.watch(path.as_ref(), RecursiveMode::Recursive) {
        log::error!("{e}");
    }
}

fn unwatch<W: Watcher, P: AsRef<Path>>(w: &mut W, path: P) {
    if let Err(e) = w.unwatch(path.as_ref()) {
        log::error!("{e}");
    }
}

fn unload() {
    unsafe {
        let arclogspath = Settings::get().logpath.clone();
        let watcher = WATCHER.take().unwrap();
        let mut watcher = watcher.write().unwrap();
        unwatch(&mut *watcher, arclogspath);
        drop(watcher);
        drop(FILEPATH_TX.take());
        drop(DPSURL_TX.take());
        let _ = Settings::take().unwrap().store(config_path());
        for t in THREADS.take().unwrap() {
            let _ = t.join();
        }
        drop(DPS_REPORT_HANDLER.take());
        drop(dpslog::UPLOADS.take());
        drop(WATCH_EVENTS_RX.take());
    }
}

fn render_fn(ui: &Ui) {
    let rx = unsafe { WATCH_EVENTS_RX.get().unwrap().lock().unwrap() };

    let ev = rx.try_recv();
    if let Ok(Ok(event)) = ev {
        if let EventKind::Modify(ModifyKind::Name(RenameMode::To)) = event.kind {
            unsafe { dpslog::UPLOADS.get_mut().unwrap() }.extend(
                event
                    .paths
                    .into_iter()
                    .filter(|p| p.is_file())
                    .filter(|p| p.extension().is_some_and(|e| e == "zevtc" || e == "evtc"))
                    .map(|f| {
                        Arc::new(Mutex::new(Upload {
                            status: UploadStatus::Pending,
                            logtype: Default::default(),
                            file: f,
                            dpsreporturl: None,
                            wingmanurl: None,
                            dpsreportobject: None,
                        }))
                    }),
            );
        }
    };

    let show_window = &mut Settings::get_mut().show_window;
    let (_w, t) = if *show_window {
        let flags = TableFlags::BORDERS_OUTER
            | TableFlags::BORDERS_INNER_V
            | TableFlags::NO_HOST_EXTEND_X
            | TableFlags::SIZING_FIXED_FIT
            | TableFlags::NO_PAD_INNER_X;
        let max_state_width = ui.calc_text_size(format!("{}", UploadStatus::DpsReportDone))[0];
        let max_path_width =
            ui.calc_text_size("Kanaxai, Scythe of House Aurkus\\20230719-194103.zevtc")[0];
        let retry_width = ui.calc_text_size("Retry")[0] + 20.0; // padding
        let w = Window::new("Wingman Uploader")
            .opened(show_window)
            .collapsible(false)
            .begin(ui);
        let t = w.as_ref().and_then(|_| {
            ui.begin_table_header_with_flags(
                "Uploads",
                [
                    TableColumnSetup {
                        name: "Status",
                        flags: TableColumnFlags::WIDTH_FIXED,
                        init_width_or_weight: max_state_width + 10.0,
                        user_id: Default::default(),
                    },
                    TableColumnSetup {
                        name: "File",
                        flags: TableColumnFlags::WIDTH_FIXED,
                        init_width_or_weight: max_path_width + 10.0,
                        user_id: Default::default(),
                    },
                    TableColumnSetup {
                        name: "",
                        flags: TableColumnFlags::WIDTH_STRETCH,
                        init_width_or_weight: max_state_width + 10.0,
                        user_id: Default::default(),
                    },
                ],
                flags,
            )
        });
        (w, t)
    } else {
        (None, None)
    };
    for upload in unsafe { dpslog::UPLOADS.get().unwrap() } {
        let mut u = upload.lock().unwrap();
        if let Some(ref _t) = t {
            u.render_row(ui);
        }
        match u.status {
            UploadStatus::Pending => {
                let _ = unsafe { FILEPATH_TX.get().unwrap() }.send(upload.clone());
            }
            UploadStatus::DpsReportDone => {
                if u.logtype == dpslog::Logtype::Pve && Settings::get().enable_wingman {
                    let _ = unsafe { DPSURL_TX.get().unwrap() }.send(upload.clone());
                } else {
                    u.status = UploadStatus::WingmanSkipped;
                }
            }
            _ => {}
        }
    }
}

fn render_options(ui: &Ui) {
    Settings::get_mut().render(ui);
}

nexus::export! {
    signature: -69421,
    load,
    unload,
    flags: AddonFlags::None,
    provider: UpdateProvider::GitHub,
    update_link: "https://github.com/belst/nexus-wingman-uploader"
}
