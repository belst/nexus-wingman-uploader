use std::{
    ffi::CStr,
    mem::MaybeUninit,
    path::{Path, PathBuf},
    ptr::{null_mut, NonNull},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex, OnceLock,
    },
    thread::JoinHandle,
    time::Duration,
};

use arcdps_imgui::{
    sys::{cty::c_char, igSetAllocatorFunctions, igSetCurrentContext},
    Context, TableColumnFlags, TableColumnSetup, TableFlags, Ui, Window,
};
use dpsreportupload::DpsReportUploader;
use nexus_rs::raw_structs::{
    AddonAPI, AddonDefinition, AddonVersion, EAddonFlags, ERenderType, Keybind,
};
use notify::{
    event::{ModifyKind, RenameMode},
    Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher,
};
use settings::Settings;
use ui::UiExt;
use ureq::AgentBuilder;
use windows::core::s;
use wingmanupload::WingmanUploader;

use crate::log::error;

mod dpsreportupload;
mod log;
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
#[derive(Debug, PartialEq, Default)]
enum UploadStatus {
    #[default]
    Pending,
    DpsReportInProgress,
    DpsReportDone,
    WingmanInProgress,
    WingmanSkipped,
    Done,
    Quit,
    Error,
}

#[derive(Debug, PartialEq, Default)]
enum Logtype {
    #[default]
    Pve,
    Wvw,
}

#[derive(Debug, Default)]
struct Upload {
    status: UploadStatus,
    logtype: Logtype,
    file: PathBuf,
    dpsreporturl: Option<String>,
    wingmanurl: Option<String>,
}

static mut API: MaybeUninit<&'static AddonAPI> = MaybeUninit::uninit();
static mut CTX: MaybeUninit<Context> = MaybeUninit::uninit();
static mut UI: MaybeUninit<Ui> = MaybeUninit::uninit();
static mut UPLOADS: OnceLock<Vec<Arc<Mutex<Upload>>>> = OnceLock::new();
static mut THREADS: OnceLock<Vec<JoinHandle<()>>> = OnceLock::new();
static FILEPATH_TX: OnceLock<Sender<Arc<Mutex<Upload>>>> = OnceLock::new();
static DPSURL_TX: OnceLock<Sender<Arc<Mutex<Upload>>>> = OnceLock::new();
static WATCH_EVENTS_RX: OnceLock<Mutex<Receiver<notify::Result<Event>>>> = OnceLock::new();
static WATCHER: OnceLock<RecommendedWatcher> = OnceLock::new();
static mut SETTINGS: OnceLock<Settings> = OnceLock::new();

unsafe fn config_path() -> PathBuf {
    let api = API.assume_init();
    let config_path = CStr::from_ptr((api.get_addon_directory)(
        s!("wingman-uploader\\settings.json").0 as _,
    ))
    .to_string_lossy()
    .into_owned();
    config_path.into()
}

unsafe extern "C" fn load(api: *mut AddonAPI) {
    let api = &*api;
    API.write(api);

    igSetCurrentContext(api.imgui_context);
    igSetAllocatorFunctions(Some(api.imgui_malloc), Some(api.imgui_free), null_mut());
    CTX.write(Context::current());
    UI.write(Ui::from_ctx(CTX.assume_init_ref()));

    let _ = UPLOADS.set(Vec::new());
    let _ = THREADS.set(Vec::new());
    let api = API.assume_init();

    let _ = SETTINGS.set(Settings::from_path(config_path()).unwrap_or_else(|_| Settings::new()));
    let token = SETTINGS.get().unwrap().dpsreport_token.clone();
    let dpsreport = if !token.is_empty() {
        DpsReportUploader::with_token(token)
    } else {
        DpsReportUploader::new()
    };
    let (filepath_tx, filepath_rx) = mpsc::channel();
    THREADS.get_mut().unwrap().push(dpsreport.run(filepath_rx));

    let (dpsurl_tx, dpsurl_rx) = mpsc::channel();
    let wingman = WingmanUploader::new();
    THREADS.get_mut().unwrap().push(wingman.run(dpsurl_rx));
    let _ = FILEPATH_TX.set(filepath_tx);
    let _ = DPSURL_TX.set(dpsurl_tx);

    let (tx, rx) = mpsc::channel();
    WATCH_EVENTS_RX.get_or_init(|| Mutex::new(rx));

    let mut watcher = RecommendedWatcher::new(tx, Config::default()).unwrap();
    let arclogspath = SETTINGS.get().unwrap().logpath.clone();
    set_watch_path(&mut watcher, arclogspath);
    let _ = WATCHER.set(watcher);

    (api.register_render)(ERenderType::Render, render);
    (api.register_render)(ERenderType::OptionsRender, render_options);
    (api.register_keybind_with_struct)(
        KB_IDENTIFIER,
        keypress,
        Keybind {
            key: 17, // W
            alt: true,
            ctrl: false,
            shift: true,
        },
    );
}

const KB_IDENTIFIER: *const c_char = s!("KB_OPEN_WINGMAN_UPLOADS").0 as _;

unsafe extern "C" fn keypress(_: *const c_char) {
    let settings = SETTINGS.get_mut().unwrap();
    settings.show_window = !settings.show_window;
}

fn set_watch_path<W: Watcher, P: AsRef<Path>>(w: &mut W, path: P) {
    let _ = w.watch(path.as_ref(), RecursiveMode::Recursive);
}

unsafe extern "C" fn unload() {
    let api = unsafe { API.assume_init() };
    let quit = Arc::new(Mutex::new(Upload {
        status: UploadStatus::Quit,
        ..Default::default()
    }));
    FILEPATH_TX.get().unwrap().send(quit.clone()).ok();
    DPSURL_TX.get().unwrap().send(quit).ok();
    let _ = SETTINGS.get().unwrap().store(config_path());
    (api.unregister_render)(render);
    (api.unregister_render)(render_options);
    // (api.unregister_keybind)(KB_IDENTIFIER);
    for t in THREADS.take().unwrap() {
        let _ = t.join();
    }
}

extern "C" fn render() {
    let rx = WATCH_EVENTS_RX.get().unwrap().lock().unwrap();

    let ev = rx.try_recv();
    if let Ok(Ok(event)) = ev {
        if let EventKind::Modify(ModifyKind::Name(RenameMode::To)) = event.kind {
            unsafe { UPLOADS.get_mut().unwrap() }.extend(
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
                        }))
                    }),
            );
        }
    };

    let show_window = unsafe { &mut SETTINGS.get_mut().unwrap().show_window };
    let ui = unsafe { UI.assume_init_ref() };
    let (w, t) = if *show_window {
        let flags = TableFlags::BORDERS_OUTER
            | TableFlags::BORDERS_INNER_V
            | TableFlags::NO_HOST_EXTEND_X
            | TableFlags::SIZING_FIXED_FIT
            | TableFlags::NO_PAD_INNER_X;
        let max_state_width =
            ui.calc_text_size(format!("{:?}", UploadStatus::DpsReportInProgress))[0];
        let max_path_width =
            ui.calc_text_size("Kanaxai, Scythe of House Aurkus\\20230719-194103.zevtc")[0];
        (
            Window::new("Wingman Uploader")
                .opened(show_window)
                .collapsible(false)
                .begin(ui),
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
                        name: "Url",
                        flags: TableColumnFlags::WIDTH_STRETCH,
                        init_width_or_weight: max_state_width + 10.0,
                        user_id: Default::default(),
                    },
                    TableColumnSetup {
                        name: "Wingman",
                        flags: TableColumnFlags::WIDTH_STRETCH,
                        init_width_or_weight: max_state_width + 10.0,
                        user_id: Default::default(),
                    },
                ],
                flags,
            ),
        )
    } else {
        (None, None)
    };
    for upload in unsafe { UPLOADS.get().unwrap() } {
        let mut u = upload.lock().unwrap();
        if let Some(ref _t) = t {
            ui.table_next_column();
            ui.text(format!("{:?}", u.status));

            ui.table_next_column();
            let file = u.file.iter().rev().take(2).fold(String::new(), |acc, c| {
                c.to_string_lossy().to_string() + "\\" + &acc
            });
            ui.text(file.trim_end_matches('\\'));

            ui.table_next_column();
            if let Err(e) = ui.link(
                u.dpsreporturl.as_deref().unwrap_or("Upload Pending"),
                u.dpsreporturl.as_ref(),
            ) {
                error(format!("Error opening browser: {e}"));
            }
            ui.table_next_column();
            let url =
                if unsafe { !SETTINGS.get().unwrap().enable_wingman } && u.wingmanurl.is_none() {
                    "Wingman uploads disabled"
                } else if u.wingmanurl.is_none() {
                    "Wingmanupload pending"
                } else {
                    u.wingmanurl.as_ref().unwrap()
                };
            if let Err(e) = ui.link(url, u.wingmanurl.as_ref()) {
                error(format!("Error opening browser: {e}"));
            }
        }
        match u.status {
            UploadStatus::Pending => {
                let _ = FILEPATH_TX.get().unwrap().send(upload.clone());
            }
            UploadStatus::DpsReportDone => {
                if u.logtype == Logtype::Pve && unsafe { SETTINGS.get().unwrap().enable_wingman } {
                    let _ = DPSURL_TX.get().unwrap().send(upload.clone());
                } else {
                    u.status = UploadStatus::WingmanSkipped;
                }
            }
            _ => {}
        }
    }
    if let Some(t) = t {
        t.end();
    }
    if let Some(w) = w {
        w.end();
    }
}

extern "C" fn render_options() {
    let ui = unsafe { UI.assume_init_ref() };
    let settings = unsafe { SETTINGS.get_mut().unwrap() };

    // TODO: make them editable
    ui.separator();
    ui.input_text("Logpath", &mut settings.logpath)
        .read_only(true)
        .build();
    ui.input_text("Dps Report Token", &mut settings.dpsreport_token)
        .read_only(true)
        .build();
    ui.checkbox("Enable Wingman?", &mut settings.enable_wingman);
}

#[no_mangle]
pub extern "C" fn GetAddonDef() -> *mut AddonDefinition {
    static AD: AddonDefinition = AddonDefinition {
        signature: -69421,
        apiversion: nexus_rs::raw_structs::NEXUS_API_VERSION,
        name: s!("Wingmanuploader").0 as _,
        version: AddonVersion {
            major: 0,
            minor: 5,
            build: 1,
            revision: 0,
        },
        author: s!("belst").0 as _,
        description: s!("Uploads Logs to dps.report and gw2wingman").0 as _,
        load,
        unload: Some(unsafe { NonNull::new_unchecked(unload as _) }),
        flags: EAddonFlags::None,
        provider: nexus_rs::raw_structs::EUpdateProvider::GitHub,
        update_link: Some(unsafe {
            NonNull::new_unchecked(s!("https://github.com/belst/nexus-wingman-uploader").0 as _)
        }),
    };

    &AD as *const _ as _
}
