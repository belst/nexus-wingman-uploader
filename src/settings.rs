use dirs_next::document_dir;
use nexus::imgui::{StyleColor, Ui};
use std::{
    fs::{create_dir_all, File},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::{e, set_watch_path, ui::UiExt, unwatch, DPS_REPORT_HANDLER, WATCHER};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Settings {
    pub logpath: String,
    pub dpsreport_token: RwLock<String>,
    #[serde(skip)]
    tmp_token: Mutex<String>,
    pub show_window: bool,
    pub enable_wingman: bool,
    #[serde(skip)]
    edit_path: bool,
    #[serde(skip)]
    display_path: String,
    #[serde(skip)]
    edit_token: bool,
    #[serde(skip)]
    invalid_path: bool,
}

static mut SETTINGS: OnceLock<Settings> = OnceLock::new();
impl Settings {
    fn default_dir() -> PathBuf {
        let mut base = document_dir().unwrap_or_default();
        base.push("Guild Wars 2");
        base.push("addons");
        base.push("arcdps");
        base.push("arcdps.cbtlogs");
        base
    }
    pub fn new() -> Self {
        let p = Self::default_dir().to_string_lossy().to_string();
        Self {
            logpath: p.clone(),
            display_path: p,
            show_window: true,
            enable_wingman: true,
            ..Default::default()
        }
    }
    pub fn get_mut() -> &'static mut Self {
        unsafe {
            if let Some(v) = SETTINGS.get_mut() {
                return v;
            }
            let _ = SETTINGS.set(Self::new());

            SETTINGS.get_mut().expect("unreachable")
        }
    }
    pub fn get() -> &'static Self {
        unsafe { SETTINGS.get_or_init(|| Self::new()) }
    }
    pub fn take() -> Option<Self> {
        unsafe { SETTINGS.take() }
    }
    pub fn from_path<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let config = std::fs::read_to_string(path)?;

        let mut s: Self = serde_json::from_str(&config)?;
        *s.tmp_token.lock().unwrap() = s.dpsreport_token.read().unwrap().clone();
        s.display_path = s.logpath.clone();

        Ok(s)
    }
    pub fn set_token(&self, token: String) {
        let mut t = self.dpsreport_token.write().unwrap();
        *self.tmp_token.lock().unwrap() = token.clone();
        *t = token;
    }
    pub fn store<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
        let path = path.as_ref();
        let prefix = path.parent().unwrap();
        create_dir_all(prefix)?;
        let mut file = File::options()
            .write(true)
            .append(false)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(serde_json::to_writer_pretty(&mut file, self)?)
    }

    pub fn render(&mut self, ui: &Ui) {
        let color = if self.invalid_path {
            Some(ui.push_style_color(StyleColor::FrameBg, [1.0, 0.0, 0.0, 1.0]))
        } else {
            None
        };
        ui.input_text("Logpath", &mut self.display_path)
            .read_only(!self.edit_path)
            .build();

        ui.same_line();
        if ui.button(if !self.edit_path {
            e("Edit") + "##pathedit"
        } else {
            e("Set") + "##pathset"
        }) {
            if self.edit_path {
                if !self.verify_path() {
                    self.invalid_path = true;
                } else {
                    self.invalid_path = false;
                    self.edit_path = false;
                    // Update watcher
                    let mut w = unsafe { WATCHER.get_mut().expect("Watcher to exist") }
                        .write()
                        .expect("Watcher to not be poisoned");
                    unwatch(&mut *w, &self.logpath);
                    self.logpath = self.display_path.clone();
                    set_watch_path(&mut *w, &self.logpath);
                }
            } else {
                self.edit_path = true;
            }
        };
        if self.invalid_path {
            ui.popover(|| ui.text_colored([1.0, 0.0, 0.0, 1.0], format!("Invalid Path")));
        }
        if let Some(color) = color {
            color.pop();
        }
        ui.input_text(
            e("Dps Report Token").as_str(),
            &mut *self.tmp_token.lock().unwrap(),
        )
        .read_only(!self.edit_token)
        .password(!self.edit_token)
        .build();
        ui.same_line();
        if ui.button(if !self.edit_token {
            e("Edit") + "##tokenedit"
        } else {
            e("Set") + "##tokenset"
        }) {
            log::debug!("Clicked token button: {}", self.edit_token);
            // TODO: this is hell. updating both from the dpsreport uploader side and the settings
            // side
            if self.edit_token {
                let token = self.tmp_token.lock().unwrap().clone();
                self.set_token(token.clone());
                let report = unsafe { DPS_REPORT_HANDLER.get().unwrap() };
                report.set_token(if token.is_empty() { None } else { Some(token) });
            }
            self.edit_token = !self.edit_token;
        };
        ui.checkbox(e("Enable Wingman?").as_str(), &mut self.enable_wingman);
    }

    fn verify_path(&self) -> bool {
        let path = Path::new(&self.display_path);
        path.is_dir()
    }
}
