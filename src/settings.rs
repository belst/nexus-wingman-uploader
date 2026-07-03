use std::{
    cell::{Cell, RefCell},
    fs::{File, create_dir_all},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
};

use dirs_next::document_dir;
use nexus::{
    imgui::{IdStackToken, StyleColor, StyleVar, Ui, Window},
    paths::get_addon_dir,
};
use serde::{Deserialize, Serialize};

use std::borrow::Cow;

use crate::{
    aleeva::{self, AleevaCommand},
    common::{GREEN, RED},
    util::{UiExt, e},
};

fn default_true() -> bool {
    true
}

fn default_copyformat() -> String {
    String::from("@1")
}

fn default_six() -> usize {
    6
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AleevaTarget {
    #[serde(default)]
    pub server_id: String,
    #[serde(default)]
    pub channel_id: String,
    #[serde(default)]
    pub send_notification: bool,
}

/// A log is considered part of the group if at least
/// `min_players` of the listed account names appear in the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AleevaGroup {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub players: Vec<String>,
    #[serde(default = "default_six")]
    pub min_players: usize,
    #[serde(default)]
    pub target: AleevaTarget,
}
thread_local! {
    pub static FRAME_NUM: Cell<u64> = const { Cell::new(0) };
    pub static LAST_OPTIONS_RENDER_TICK: Cell<u64> = const { Cell::new(0) };
    static DIRTY: Cell<bool> = const { Cell::new(false) };
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
    #[serde(default)]
    pub rev_log_order: bool,
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
    #[serde(default)]
    pub hotfix_20250512_executed: bool,
    #[serde(default)]
    pub enable_aleeva: bool,
    #[serde(default)]
    pub aleeva_api_key: String,
    /// Default aleeva target, used when no group matches, or always depending on [`settings::AleevaSettings::default_posts_unmatched_only`].
    #[serde(default)]
    pub aleeva_selected_server_id: String,
    #[serde(default)]
    pub aleeva_selected_channel_id: String,
    #[serde(default)]
    pub aleeva_send_notification: bool,
    #[serde(default)]
    pub aleeva_groups: Vec<AleevaGroup>,
    /// When `true`, the default target only receives logs that did not match
    /// any group. When `false` (the default), the default target receives all
    /// logs regardless of group matches.
    #[serde(default)]
    pub aleeva_default_posts_unmatched_only: bool,
}

impl Settings {
    const fn default() -> Self {
        Self {
            // Cannot use default_dir() because it's not consat
            logpath: String::new(),
            dpsreport_token: String::new(),
            // Cannot use default_copyformat() because it's not const
            dpsreport_copyformat: String::new(),
            show_window: true,
            rev_log_order: false,
            copy_success: true,
            copy_failure: true,
            enable_dpsreport: true,
            enable_wingman: true,
            filter_wingman: Vec::new(),
            filter_dpsreport: Vec::new(),
            hide_hotfix_notification_20241114: false,
            hotfix_20250512_executed: false,
            enable_aleeva: false,
            aleeva_api_key: String::new(),
            aleeva_selected_server_id: String::new(),
            aleeva_selected_channel_id: String::new(),
            aleeva_send_notification: false,
            aleeva_groups: Vec::new(),
            aleeva_default_posts_unmatched_only: false,
        }
    }

    pub fn init(&mut self) {
        self.logpath = Self::default_dir().display().to_string();
        self.dpsreport_copyformat = default_copyformat();
    }

    pub fn get() -> MutexGuard<'static, Self> {
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

    // Default was an empty string if config file did not exist yet
    // If the file existed (even if empty) it worked correctly
    fn check_hotfix20250512(&self) -> bool {
        self.dpsreport_copyformat.is_empty()
    }

    pub fn fix_hotfix20250512(&mut self) {
        // Only do this once so if someone actually uses an empty copyformat we wont overwrite it
        // next restart
        if !self.hotfix_20250512_executed && self.check_hotfix20250512() {
            log::info!("Empty copyformat detected, setting default (Hotfix 20250512)");
            self.dpsreport_copyformat = default_copyformat();
        }
        // Always set this to true so we don't run this again
        self.hotfix_20250512_executed = true;
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
            let mut settings: Self = serde_json::from_str(&contents)?;
            settings.fix_hotfix20250512();
            *SETTINGS.lock().unwrap() = settings;
        } else {
            // Need to set here because it's not const
            let mut settings = SETTINGS.lock().unwrap();
            settings.init();
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
        DIRTY.set(false);
        Ok(())
    }

    pub fn render_reminder(&self, ui: &Ui) {
        const FRAME_WAIT: u64 = 10;
        if FRAME_NUM.get() - LAST_OPTIONS_RENDER_TICK.get() > FRAME_WAIT && DIRTY.get() {
            Window::new(e("Log Uploader: Unsaved changes")).build(ui, || {
                ui.text(e("You have unsaved changes. Would you like to save them?"));
                if ui.button(e("Save")) {
                    if let Err(e) = self.store(config_path()) {
                        log::error!("Failed to store settings: {e}");
                    }
                }
                ui.same_line();
                if ui.button(e("Don't save")) {
                    DIRTY.set(false);
                }
            });
        }
    }
}

pub(crate) fn config_path() -> PathBuf {
    get_addon_dir("wingman-uploader")
        .expect("Addon dir to exist")
        .join("settings.json")
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
    LAST_OPTIONS_RENDER_TICK.set(FRAME_NUM.get());

    let valid = PATH_VALID.get() && !PATH_EDIT.get() && !EDIT_TOKEN.get() && !EDIT_COPYFORMAT.get();
    let stylevar = if !valid {
        Some(ui.push_style_var(StyleVar::Alpha(0.5)))
    } else {
        None
    };
    if ui.button(e("Save") + "##saveconfig") && valid {
        let settings = SETTINGS.lock().unwrap();
        log::trace!("Storing config");
        if let Err(e) = settings.store(config_path()) {
            log::error!("Failed to store settings: {e}");
        }
    }
    if let Some(stylevar) = stylevar {
        stylevar.end();
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
                    if settings.logpath != *lp {
                        settings.logpath = lp.clone();
                        DIRTY.set(true);
                    }
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
                if settings.dpsreport_token != *token {
                    settings.dpsreport_token = token.clone();
                    DIRTY.set(true);
                }
            });
        }
        EDIT_TOKEN.set(!EDIT_TOKEN.get())
    }

    DPSREPORT_COPYFORMAT.with_borrow_mut(|copyformat| {
        ui.input_text(e("dps.report copy format"), copyformat)
            .read_only(!EDIT_COPYFORMAT.get())
            .build();
    });
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
                if settings.dpsreport_copyformat != *copyformat {
                    settings.dpsreport_copyformat = copyformat.clone();
                    DIRTY.set(true);
                }
            });
        }
        EDIT_COPYFORMAT.set(!EDIT_COPYFORMAT.get())
    }
    if ui.checkbox(e("Display new logs at top"), &mut settings.rev_log_order) {
        DIRTY.set(true);
    }

    ui.separator();
    if ui.checkbox(e("Enable dps.report"), &mut settings.enable_dpsreport) {
        DIRTY.set(true);
    }
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
    if ui.checkbox(e("Enable Wingman"), &mut settings.enable_wingman) {
        DIRTY.set(true);
    }
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
    ui.separator();
    // aleeva
    render_aleeva(ui, &mut settings);
}

/// Render server + channel dropdowns and a "Send notification" checkbox for a
/// single [`AleevaTarget`].
/// `push_token` is just a convenience to prevent misuse of this function.
/// It's to make sure that a unique id was pushed (hopefully)
fn render_aleeva_target(
    ui: &Ui,
    target: &mut AleevaTarget,
    state: &aleeva::AleevaState,
    _push_token: &IdStackToken<'_>,
) {
    let servers = &state.servers;
    if servers.is_empty() {
        return;
    }
    let mut server_idx = servers
        .iter()
        .position(|s| s.id == target.server_id)
        .unwrap_or(0);
    if ui.combo(e("Server") + "##server", &mut server_idx, servers, |s| {
        Cow::from(s.name.as_str())
    }) {
        target.server_id = servers[server_idx].id.clone();
        target.channel_id.clear();
        DIRTY.set(true);
        aleeva::send(AleevaCommand::FetchChannels(servers[server_idx].id.clone()));
    }

    if let Some(server) = servers.get(server_idx) {
        if server.channels.is_empty() {
            ui.text(e("No channels loaded for this server."));
        } else {
            let mut chan_idx = server
                .channels
                .iter()
                .position(|c| c.id == target.channel_id)
                .unwrap_or(0);
            if ui.combo(
                e("Channel") + "##channel",
                &mut chan_idx,
                &server.channels,
                |c| Cow::from(c.name.as_str()),
            ) {
                target.channel_id = server.channels[chan_idx].id.clone();
                DIRTY.set(true);
            }
        }
    }

    if ui.checkbox(
        e("Send notification") + "##sendnotification",
        &mut target.send_notification,
    ) {
        DIRTY.set(true);
    }
}

fn render_aleeva(ui: &Ui, settings: &mut Settings) {
    thread_local! {
        static API_KEY: RefCell<String> = const { RefCell::new(String::new()) };
        static EDIT_KEY: Cell<bool> = const { Cell::new(false) };
        static INITIALIZED: Cell<bool> = const { Cell::new(false) };
        /// Per-group "add player" input buffers, indexed by group position.
        static PLAYER_INPUTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
        /// Input buffer for the "new group" name field.
        static NEW_GROUP_NAME: RefCell<String> = const { RefCell::new(String::new()) };
    }
    if !INITIALIZED.get() {
        API_KEY.set(settings.aleeva_api_key.clone());
        INITIALIZED.set(true);
    }

    if ui.checkbox(e("Enable Aleeva"), &mut settings.enable_aleeva) {
        DIRTY.set(true);
    }
    ui.text(e(
        "Aleeva posts the dps.report permalink, so dps.report must be enabled.",
    ));

    API_KEY.with_borrow_mut(|code| {
        ui.input_text(e("Aleeva API Key"), code)
            .read_only(!EDIT_KEY.get())
            .password(!EDIT_KEY.get())
            .build();
        ui.same_line();
        if ui.help_marker(|| ui.tooltip_text("Use /profile in discord to manage your API access. (click to open documentation for plenbot)")) {
            if let Err(e) = open::that_detached("https://www.aleeva.io/tutorials-blog/how-to-connect-plenbot-log-uploader-to-aleeva") {
                log::error!("Failed to open browser: {e}");
            }
        }
    });
    ui.same_line();
    if ui.button(if !EDIT_KEY.get() {
        e("Edit") + "##editaleevacode"
    } else {
        e("Set") + "##setaleevacode"
    }) {
        if EDIT_KEY.get() {
            API_KEY.with_borrow(|code| {
                if settings.aleeva_api_key != *code {
                    settings.aleeva_api_key = code.clone();
                    DIRTY.set(true);
                }
            });
        }
        EDIT_KEY.set(!EDIT_KEY.get());
    }

    let state = aleeva::snapshot();
    if let Some(err) = &state.last_error {
        ui.text_colored(RED, err.as_str());
    }

    if ui.button(e("Verify") + "##aleevalogin") {
        if !state.verifying {
            aleeva::send(AleevaCommand::Verify);
        }
    }
    ui.same_line();
    if state.verifying {
        ui.text_disabled(e("Verifying..."));
    } else if state.authorised {
        ui.text_colored(GREEN, e("Verified"));
    } else {
        ui.text_colored(RED, e("Not verified"));
    }

    if !state.authorised || state.servers.is_empty() {
        return;
    }

    ui.separator();
    ui.text(e("Groups"));
    ui.help_marker(|| {
        ui.tooltip(|| {
            ui.text("A log is posted to a group's target channel when at least");
            ui.text("'Min players' of the group's account names appear in the log.");
        })
    });

    // Ensure the per-group input-buffer vec stays in sync with the group list.
    PLAYER_INPUTS.with_borrow_mut(|inputs| {
        let n = settings.aleeva_groups.len();
        if inputs.len() < n {
            inputs.resize(n, String::new());
        }
    });

    let mut groups_to_remove: Vec<usize> = Vec::new();

    for (gi, group) in settings.aleeva_groups.iter_mut().enumerate() {
        let pid = ui.push_id(gi as i32);

        // Header: "GroupName  (players: N/min M)  [Remove]"
        let header_label = format!(
            "{} (players: {}, min: {})###grphdr",
            group.name,
            group.players.len(),
            group.min_players
        );
        let open = ui.collapsing_header(
            &header_label,
            nexus::imgui::TreeNodeFlags::ALLOW_ITEM_OVERLAP,
        );
        ui.same_line();
        if ui.small_button(e("Remove") + "##grpremove") {
            groups_to_remove.push(gi);
        }

        if open {
            // Name
            if ui
                .input_text(e("Name") + "##grpname", &mut group.name)
                .build()
            {
                DIRTY.set(true);
            }

            // Min players
            let mut min = group.min_players as i32;
            if ui
                .input_int(e("Min players") + "##grpmin", &mut min)
                .build()
            {
                if group.min_players != (min.max(1)) as usize {
                    group.min_players = (min.max(1)) as usize;
                    DIRTY.set(true);
                }
            }

            // Player list
            ui.text(e("Players:"));
            let mut players_to_remove: Vec<usize> = Vec::new();
            for (pi, player) in group.players.iter().enumerate() {
                let _ppid = ui.push_id(pi as i32);
                ui.text(player.as_str());
                ui.same_line();
                if ui.small_button(e("x") + "##rmplayer") {
                    players_to_remove.push(pi);
                }
            }
            for pi in players_to_remove.into_iter().rev() {
                group.players.remove(pi);
                DIRTY.set(true);
            }

            // Add-player input
            PLAYER_INPUTS.with_borrow_mut(|inputs| {
                if let Some(buf) = inputs.get_mut(gi) {
                    ui.input_text("##newplayer", buf).build();
                    ui.same_line();
                    if ui.button(e("Add player") + "##addplayer") && !buf.is_empty() {
                        group.players.push(buf.clone());
                        buf.clear();
                        DIRTY.set(true);
                    }
                }
            });

            // Target
            ui.text(e("Post target:"));
            render_aleeva_target(ui, &mut group.target, &state, &pid);
        }
    }

    // Remove groups in reverse order to keep indices valid.
    for gi in groups_to_remove.into_iter().rev() {
        settings.aleeva_groups.remove(gi);
        PLAYER_INPUTS.with_borrow_mut(|inputs| {
            if gi < inputs.len() {
                inputs.remove(gi);
            }
        });
        DIRTY.set(true);
    }

    // "Add group" row
    ui.separator();
    NEW_GROUP_NAME.with_borrow_mut(|name| {
        ui.input_text("##newgroupname", name).build();
        ui.same_line();
        if ui.button(e("Add group") + "##addgroup") && !name.is_empty() {
            settings.aleeva_groups.push(AleevaGroup {
                name: name.clone(),
                players: Vec::new(),
                min_players: 1,
                target: AleevaTarget::default(),
            });
            name.clear();
            DIRTY.set(true);
        }
    });

    // Default target
    ui.separator();
    ui.text(e("Default target"));
    ui.help_marker(|| {
        ui.tooltip(|| {
            ui.text("The default target receives logs that don't match any group");
            ui.text("(or all logs, depending on the toggle below).");
        })
    });
    if ui.checkbox(
        e("Only post logs to default target that didn't match any group"),
        &mut settings.aleeva_default_posts_unmatched_only,
    ) {
        DIRTY.set(true);
    }
    // Reuse the existing server/channel/notify fields as the default target.
    let mut default_target = AleevaTarget {
        server_id: settings.aleeva_selected_server_id.clone(),
        channel_id: settings.aleeva_selected_channel_id.clone(),
        send_notification: settings.aleeva_send_notification,
    };
    {
        let pid = ui.push_id("default_aleeva_target");
        render_aleeva_target(ui, &mut default_target, &state, &pid);
    }
    if default_target.server_id != settings.aleeva_selected_server_id {
        settings.aleeva_selected_server_id = default_target.server_id;
        DIRTY.set(true);
    }
    if default_target.channel_id != settings.aleeva_selected_channel_id {
        settings.aleeva_selected_channel_id = default_target.channel_id;
        DIRTY.set(true);
    }
    if default_target.send_notification != settings.aleeva_send_notification {
        settings.aleeva_send_notification = default_target.send_notification;
        DIRTY.set(true);
    }
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
    if !to_remove.is_empty() {
        DIRTY.set(true);
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
        DIRTY.set(true);
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
    if !to_remove.is_empty() {
        DIRTY.set(true);
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
    ui.input_int(e("ID") + "##wingmanfilterinput", &mut id)
        .build();
    ID.set(id);
    ui.table_next_column();
    if ui.button(e("Add") + "##wingmanfilterid") {
        filter.push(id as u16);
        DIRTY.set(true);
    }
}
