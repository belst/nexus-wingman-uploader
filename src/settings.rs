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

fn default_copyformat() -> String {
    String::from("@1")
}

// serde defaults only for the case, the file exists, but doesnt contain all the fields
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    pub logpath: String,
    pub dpsreport_token: String,
    #[serde(default = "default_copyformat")]
    pub dpsreport_copyformat: String,
    #[serde(default)]
    pub show_window: bool,
    #[serde(default = "default_true")]
    pub copy_success: bool,
    #[serde(default = "default_true")]
    pub copy_failure: bool,
    #[serde(default = "default_true")]
    pub enable_dpsreport: bool,
    #[serde(default = "default_true")]
    pub enable_wingman: bool,
    #[serde(default)]
    pub filter_dpsreport: Vec<u16>,
    #[serde(default)]
    pub filter_wingman: Vec<u16>,
    #[serde(default)]
    pub hide_hotfix_notification_20241114: bool,
}

impl Settings {
    const fn default() -> Self {
        Self {
            logpath: String::new(),
            dpsreport_token: String::new(),
            dpsreport_copyformat: String::new(),
            show_window: true,
            copy_success: true,
            copy_failure: true,
            enable_dpsreport: true,
            enable_wingman: true,
            filter_wingman: Vec::new(),
            filter_dpsreport: Vec::new(),
            hide_hotfix_notification_20241114: false,
        }
    }

    pub fn get() -> MutexGuard<'static, Self> {
        SETTINGS.lock().unwrap()
    }
    pub fn get_mut() -> MutexGuard<'static, Self> {
        SETTINGS.lock().unwrap()
    }

    pub fn default_dir() -> PathBuf {
        let mut base = document_dir().unwrap_or_default();
        base.push("Guild Wars 2");
        base.push("addons");
        base.push("arcdps");
        base.push("arcdps.cbtlogs");
        base
    }

    pub fn check_hotfix20241114(&self) -> bool {
        self.logpath.ends_with("arcdps.cbtlog")
    }

    pub fn fix_hotfix20241114(&mut self) {
        self.logpath = Settings::default_dir().to_string_lossy().to_string();
    }

    pub fn enable_dpsreport(&self) -> bool {
        self.enable_dpsreport
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
        static LOGPATH: RefCell<String> = const { RefCell::new(String::new()) };
        static PATH_VALID: Cell<bool> = const { Cell::new(true) };
        static PATH_EDIT: Cell<bool> = const { Cell::new(false) };
        static DPSREPORT_TOKEN: RefCell<String> = const { RefCell::new(String::new()) };
        static DPSREPORT_COPYFORMAT: RefCell<String> = const { RefCell::new(String::new()) };
        static FILTER_WINGMAN: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
        static FILTER_DPSREPORT: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
        static EDIT_TOKEN: Cell<bool> = const { Cell::new(false) };
        static EDIT_COPYFORMAT: Cell<bool> = const { Cell::new(false) };
        static INITIALIZED: Cell<bool> = const { Cell::new(false) };
    }

    if !INITIALIZED.get() {
        let settings = SETTINGS.lock().unwrap();
        LOGPATH.set(settings.logpath.clone());
        DPSREPORT_TOKEN.set(settings.dpsreport_token.clone());
        DPSREPORT_COPYFORMAT.set(settings.dpsreport_copyformat.clone());
        FILTER_WINGMAN.set(settings.filter_wingman.clone());
        FILTER_DPSREPORT.set(settings.filter_dpsreport.clone());
        INITIALIZED.set(true);
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

    DPSREPORT_COPYFORMAT.with_borrow_mut(|copyformat| {
        ui.input_text(e("dps.report copy format"), copyformat)
            .read_only(!EDIT_COPYFORMAT.get())
            .build();
        ui.help_marker(|| {
            ui.tooltip(|| {
                ui.text(
                    "You can configure the format that your dps.report url strings are copied as using the following parameters:",
                );
                ui.text("@1 - dps.report url");
                ui.text("@2 - boss name and CM status");
                ui.text("@3 - boss id");
                ui.text("@4 - encounter success/fail");
            })
        });
    });
    ui.same_line();
    if ui.button(if !EDIT_COPYFORMAT.get() {
        e("Edit") + "##editcopyformat"
    } else {
        e("Set") + "##setcopyformat"
    }) {
        // button got clicked, check current state and toggle it
        if EDIT_COPYFORMAT.get() {
            // Set button was clicked
            DPSREPORT_COPYFORMAT.with_borrow(|copyformat| {
                settings.dpsreport_copyformat = copyformat.clone();
            });
        }
        EDIT_COPYFORMAT.set(!EDIT_COPYFORMAT.get())
    }

    ui.separator();
    ui.checkbox(e("Enable dps.report"), &mut settings.enable_dpsreport);
    ui.text("Don't upload logs to dps.report with the following boss ids:");
    if ui.help_marker(|| {
        ui.tooltip(|| {
            ui.text(
                "You can check your log folder for the boss ids. It is the number in parentheses.",
            );
            ui.text("For example: Gorseval the Multifarious (15429)");
            ui.text("The boss id would be 15429.");
            ui.text("Click to open log folder.");
        })
    }) {
        if let Err(e) = open::that_detached(&settings.logpath) {
            log::error!("Failed to open log folder: {e}");
        }
    }
    render_dpsreport_filter(ui, &mut settings.filter_dpsreport);
    ui.separator();
    // wingman
    ui.checkbox(e("Enable Wingman"), &mut settings.enable_wingman);
    ui.text("Don't upload logs to Wingman with the following boss ids:");
    if ui.help_marker(|| {
        ui.tooltip(|| {
            ui.text(
                "You can check your log folder for the boss ids. It is the number in parentheses.",
            );
            ui.text("For example: Large Kitty Golem (19676)");
            ui.text("The boss id would be 19676.");
            ui.text("WvW logs are skipped by default. (ID: 1)");
            ui.text("Click to open log folder.");
        })
    }) {
        if let Err(e) = open::that_detached(&settings.logpath) {
            log::error!("Failed to open log folder: {e}");
        }
    }
    render_wingman_filter(ui, &mut settings.filter_wingman);
}

fn render_dpsreport_filter(ui: &Ui, filter: &mut Vec<u16>) {
    let _t = ui.begin_table("dpsreport filter", 2);
    let mut to_remove = Vec::new();
    for (i, id) in filter.iter().enumerate() {
        ui.table_next_row();
        ui.table_next_column();
        ui.text(format!("{}", id));
        ui.table_next_column();
        if ui.button(e("remove") + &format!("##dpsremove{i}")) {
            to_remove.push(i);
        }
    }
    for tr in to_remove {
        filter.remove(tr);
    }
    ui.table_next_row();
    ui.table_next_column();
    thread_local! {
        static ID: Cell<i32> = const { Cell::new(0) };
    }
    let mut id = ID.get();
    ui.input_int(e("ID##dpsreportfilterinput"), &mut id).build();
    ID.set(id);
    ui.table_next_column();
    if ui.button(e("Add##dpsreportfilterid")) {
        filter.push(id as u16);
    }
}
fn render_wingman_filter(ui: &Ui, filter: &mut Vec<u16>) {
    let _t = ui.begin_table("wingman filter", 2);
    let mut to_remove = Vec::new();
    for (i, id) in filter.iter().enumerate() {
        ui.table_next_row();
        ui.table_next_column();
        ui.text(format!("{}", id));
        ui.table_next_column();
        if ui.button(e("remove") + &format!("##wingmanfilterremove{i}")) {
            to_remove.push(i);
        }
    }
    for tr in to_remove {
        filter.remove(tr);
    }
    ui.table_next_row();
    ui.table_next_column();
    thread_local! {
        static ID: Cell<i32> = const { Cell::new(0) };
    }
    let mut id = ID.get();
    ui.input_int(e("ID##wingmanfilterinput"), &mut id).build();
    ID.set(id);
    ui.table_next_column();
    if ui.button(e("Add##wingmanfilterid")) {
        filter.push(id as u16);
    }
}
