use std::{
    cell::{Cell, RefCell},
    fs::{create_dir_all, File},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use dirs_next::document_dir;
use nexus::imgui::{StyleColor, Ui};
use serde::{Deserialize, Serialize};

use crate::{
    common::RED,
    util::{e, UiExt},
};

fn default_true() -> bool {
    true
}

// serde defaults only for the case, the file exists, but doesnt contain all the fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub logpath: String,
    pub dpsreport_token: String,
    #[serde(default)]
    pub show_window: bool,
    #[serde(default = "default_true")]
    pub enable_dpsreport: bool,
    #[serde(default = "default_true")]
    pub enable_wingman: bool,
}

impl Settings {
    const fn default() -> Self {
        Self {
            logpath: String::new(),
            dpsreport_token: String::new(),
            show_window: true,
            enable_dpsreport: true,
            enable_wingman: true,
        }
    }

    pub fn get() -> MutexGuard<'static, Self> {
        SETTINGS.lock().unwrap()
    }
    pub fn get_mut() -> MutexGuard<'static, Self> {
        SETTINGS.lock().unwrap()
    }

    fn default_dir() -> PathBuf {
        let mut base = document_dir().unwrap_or_default();
        base.push("Guild Wars 2");
        base.push("addons");
        base.push("arcdps");
        base.push("arcdps.cbtlog");
        base
    }

    pub fn dpsreport_token(&self) -> &str {
        &self.dpsreport_token
    }

    pub fn show_window(&self) -> bool {
        self.show_window
    }

    pub fn enable_dpsreport(&self) -> bool {
        self.enable_dpsreport
    }

    pub fn enable_wingman(&self) -> bool {
        self.enable_wingman
    }

    pub fn logpath(&self) -> &str {
        &self.logpath
    }

    pub fn from_path(path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        if path.exists() {
            let contents = std::fs::read_to_string(path)?;
            let settings: Self = serde_json::from_str(&contents)?;
            *SETTINGS.lock().unwrap() = settings;
        } else {
            SETTINGS.lock().unwrap().logpath = Self::default_dir().display().to_string();
        }
        Ok(())
    }

    pub fn store(&self, path: impl AsRef<Path>) -> anyhow::Result<()> {
        let path = path.as_ref();
        let prefix = path.parent().unwrap();
        create_dir_all(prefix)?;
        let mut file = File::options()
            .write(true)
            .append(false)
            .create(true)
            .truncate(true)
            .open(path)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        Ok(())
    }
}

static SETTINGS: Mutex<Settings> = Mutex::new(Settings::default());

fn validate_path(path: &str) -> bool {
    let path = Path::new(path);
    path.is_dir()
}

pub fn render(ui: &Ui) {
    thread_local! {
        static LOGPATH: RefCell<String> = RefCell::new(SETTINGS.lock().unwrap().logpath.clone());
        static PATH_VALID: Cell<bool> = Cell::new(true);
        static PATH_EDIT: Cell<bool> = Cell::new(false);
        static DPSREPORT_TOKEN: RefCell<String> = RefCell::new(SETTINGS.lock().unwrap().dpsreport_token.clone());
        static EDIT_TOKEN: Cell<bool> = Cell::new(false);
    }

    let color = if !PATH_VALID.get() {
        Some(ui.push_style_color(StyleColor::FrameBg, RED))
    } else {
        None
    };
    // logpath
    LOGPATH.with_borrow_mut(|lp| {
        ui.input_text("Logpath", lp)
            .read_only(!PATH_EDIT.get())
            .build()
    });
    if let Some(color) = color {
        color.end();
    }
    ui.same_line();
    if ui.button(if !PATH_EDIT.get() {
        e("Edit") + "##pathedit"
    } else {
        e("Set") + "##pathset"
    }) {
        // button got clicked, check current state and toggle it
        if PATH_EDIT.get() {
            // Set button was clicked, so we need to validate the path
            LOGPATH.with_borrow(|lp| {
                if !validate_path(lp.as_str()) {
                    PATH_VALID.set(false);
                } else {
                    PATH_VALID.set(true);
                    // we are done editing
                    PATH_EDIT.set(false);

                    let mut settings = SETTINGS.lock().unwrap();
                    settings.logpath = lp.clone();
                }
            });
        } else {
            PATH_EDIT.set(true);
        }
        if !PATH_VALID.get() {
            ui.attention_marker(|| ui.text_colored(RED, e("Invalid path")));
        }
    }
    // dpsreport
    let mut settings = SETTINGS.lock().unwrap();
    DPSREPORT_TOKEN.with_borrow_mut(|token| {
        if !EDIT_TOKEN.get() && token.as_str() != settings.dpsreport_token.as_str() {
            // we are not editing but token changed
            // can only happen if dps report response was successful
            // Update local input token
            *token = settings.dpsreport_token.clone();
        }
        ui.input_text(e("dps.report Token"), token)
            .read_only(!EDIT_TOKEN.get())
            .password(!EDIT_TOKEN.get())
            .build();
    });
    ui.same_line();
    if ui.button(if !EDIT_TOKEN.get() {
        e("Edit") + "##edittoken"
    } else {
        e("Set") + "##settoken"
    }) {
        // button got clicked, check current state and toggle it
        if EDIT_TOKEN.get() {
            // Set button was clicked
            DPSREPORT_TOKEN.with_borrow(|token| {
                settings.dpsreport_token = token.clone();
            });
        }
        EDIT_TOKEN.set(!EDIT_TOKEN.get())
    }
    ui.checkbox(e("Enable dps.report"), &mut settings.enable_dpsreport);
    // wingman
    ui.checkbox(e("Enable Wingman"), &mut settings.enable_wingman);
}
