use anyhow::{Error, Result};
use chrono::DateTime;
use chrono::Local;
use nexus::imgui::Image;
use nexus::imgui::ImageButton;
use nexus::imgui::MouseButton;
use nexus::imgui::Ui;
use nexus::texture::get_texture;
use revtc::{bossdata::BossId, evtc::Encounter};
use std::cell::Cell;
use std::time::Instant;
use std::{path::PathBuf, time::SystemTime};

use crate::assets::DPSREPORT;
use crate::assets::OPEN_IN_FOLDER;
use crate::assets::WINGMAN;
use crate::common::GREEN;
use crate::common::RED;
use crate::dpsreport::DpsReportResponse;
use crate::evtc::identifier_from_agent;
use crate::util;
use crate::util::UiExt;
use crate::util::e;

// Maybe this needs a retry option for retryable errors
#[derive(Debug)]
pub enum Step<T> {
    Pending,
    Active,
    Done(T),
    Skipped,
    Error(Error),
    Retry(Instant),
}

impl<T> Step<T> {
    pub fn from_value(value: Result<T>) -> Self {
        match value {
            Ok(v) => Self::Done(v),
            Err(e) => Self::Error(e),
        }
    }
}

pub struct Log {
    pub location: PathBuf,
    pub evtc: Step<Encounter>,
    pub dpsreport: Step<DpsReportResponse>,
    pub dpsreport_count: u32,
    pub wingman: Step<bool>,
}

fn format_time(time: SystemTime) -> String {
    let dt = DateTime::<Local>::from(time);
    format!("{}", dt.format("%R"))
}

const PULSE_SPEED: f32 = 5.0;
fn pulse(t: f32) -> f32 {
    let t = t * PULSE_SPEED;
    (1.0 + t.sin()) * 0.5
}

impl Log {
    pub fn new(location: PathBuf) -> Self {
        use Step as S;
        Self {
            location,
            evtc: S::Pending,
            dpsreport: S::Pending,
            dpsreport_count: 0,
            wingman: S::Pending,
        }
    }

    fn basename(&self) -> String {
        self.location
            .parent()
            .and_then(|p| p.file_name())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    fn render_dpsreport(&self, ui: &Ui) {
        thread_local! {
            static TS: Cell<Instant> = Cell::new(Instant::now());
        }
        let Some(tex) = get_texture(DPSREPORT) else {
            return;
        };

        // TODO errorhandling
        match &self.dpsreport {
            Step::Done(dpsreport) => {
                let push_id =
                    ui.push_id(format!("{}btn_dpsreport", self.location.display()).as_str());
                if ImageButton::new(tex.id(), [16.0, 16.0])
                    .frame_padding(0)
                    .build(ui)
                {
                    if let Err(e) = open::that_detached(&dpsreport.permalink) {
                        log::error!("Failed to open browser: {e}");
                    }
                }
                push_id.end();
                if ui.is_item_hovered() {
                    ui.tooltip_text(e("Open log in Browser (Rightclick to copy)"));
                    if ui.is_mouse_clicked(MouseButton::Right) {
                        // replace with url
                        ui.set_clipboard_text(&dpsreport.permalink);
                    }
                }
            }
            Step::Error(err) => {
                let mut red = RED;
                red[3] = 0.3;
                Image::new(tex.id(), [16.0, 16.0]).tint_col(red).build(ui);
                if ui.is_item_hovered() {
                    ui.tooltip_text(e("Error uploading to dps.report: ") + &format!("{err}"));
                }
            }
            Step::Pending | Step::Active | Step::Retry(_) => {
                Image::new(tex.id(), [16.0, 16.0])
                    .tint_col([1.0, 1.0, 1.0, pulse(TS.get().elapsed().as_secs_f32())])
                    .build(ui);
                if ui.is_item_hovered() {
                    if let Step::Retry(t) = self.dpsreport {
                        ui.tooltip_text(
                            e("Retrying in ")
                                + &format!("{}", (Instant::now() - t).as_secs())
                                + " seconds",
                        );
                    }
                    ui.tooltip_text(e(if matches!(self.dpsreport, Step::Active) {
                        "Uploading..."
                    } else {
                        "Queued"
                    }));
                }
            }
            Step::Skipped => {
                Image::new(tex.id(), [16.0, 16.0])
                    .tint_col([1.0, 1.0, 1.0, 0.3])
                    .build(ui);
                if ui.is_item_hovered() {
                    ui.tooltip_text(e("Skipped"));
                }
            }
        }
    }

    fn render_open_in_folder(&self, ui: &Ui) {
        let Some(tex) = get_texture(OPEN_IN_FOLDER) else {
            return;
        };
        let push_id = ui.push_id(format!("{}open_in_folder_btn", self.location.display()).as_str());
        if ImageButton::new(tex.id(), [16.0, 16.0])
            .frame_padding(0)
            .build(ui)
        {
            if let Err(e) = util::open_with_selected(&self.location) {
                log::error!("Failed to open folder: {e}");
            }
        }
        push_id.end();
        if ui.is_item_hovered() {
            ui.tooltip_text(e("Show Log in Folder"));
        }
    }

    fn render_wingman(&self, ui: &Ui) {
        thread_local! {
            static TS: Cell<Instant> = Cell::new(Instant::now());
        }
        let Some(tex) = get_texture(WINGMAN) else {
            return;
        };
        match &self.wingman {
            Step::Done(wingman) => {
                Image::new(tex.id(), [16.0, 16.0])
                    .tint_col(if *wingman {
                        // dont tint on success
                        [1.0, 1.0, 1.0, 1.0]
                    } else {
                        let mut red = RED;
                        red[3] = 0.3;
                        red
                    })
                    .build(ui);
                if ui.is_item_hovered() {
                    ui.tooltip_text(e(if *wingman {
                        "Log queued for Wingman"
                    } else {
                        "Error queueing for Log"
                    }));
                }
            }
            Step::Skipped => {
                Image::new(tex.id(), [16.0, 16.0])
                    .tint_col([1.0, 1.0, 1.0, 0.3])
                    .build(ui);
                if ui.is_item_hovered() {
                    ui.tooltip_text(e("Skipped"));
                }
            }
            Step::Active | Step::Pending => {
                Image::new(tex.id(), [16.0, 16.0])
                    .tint_col([1.0, 1.0, 1.0, pulse(TS.get().elapsed().as_secs_f32())])
                    .build(ui);
                if ui.is_item_hovered() {
                    ui.tooltip_text(e(if matches!(self.wingman, Step::Active) {
                        "Uploading..."
                    } else {
                        "Queued"
                    }));
                }
            }
            Step::Error(err) => {
                let mut red = RED;
                red[3] = 0.3;
                Image::new(tex.id(), [16.0, 16.0]).tint_col(red).build(ui);
                if ui.is_item_hovered() {
                    ui.tooltip_text(e("Error uploading to wingman: ") + &format!("{err}"));
                }
            }
            Step::Retry(_t) => {
                // Not supported
            }
        }
    }

    pub fn render_row(&self, ui: &Ui) {
        // Encounter
        ui.table_next_column();
        let hovered = if let Step::Done(evtc) = &self.evtc {
            self.render_title(ui, evtc)
        } else {
            ui.text(self.basename().as_str());
            ui.is_item_hovered()
        };
        if hovered {
            self.render_hovered(ui);
        }
        // Timestamp
        ui.table_next_column();
        ui.text(
            self.location
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(format_time)
                .unwrap_or_default(),
        );
        // DpsReport
        ui.table_next_column();
        self.render_dpsreport(ui);
        // Wingman
        ui.table_next_column();
        self.render_wingman(ui);
        // Open in Folder
        ui.table_next_column();
        self.render_open_in_folder(ui);
    }

    // Returns wether the text was hovered
    fn render_title(&self, ui: &Ui, evtc: &Encounter) -> bool {
        let hovered;
        if let Step::Done(dpsreport) = &self.dpsreport {
            let color = if dpsreport.encounter.success {
                GREEN
            } else {
                RED
            };
            if let Some(mode) = dpsreport.encounter.format_mode() {
                if mode == "" {
                    ui.text_colored(color, format!("{}", dpsreport.encounter.boss));
                } else {
                    ui.text_colored(color, format!("{} ({})", dpsreport.encounter.boss, mode));
                }
                hovered = ui.is_item_hovered();
            } else {
                ui.text_colored(color, &dpsreport.encounter.boss);
                // needs to be before the help_marker because it also has a hovered check
                hovered = ui.is_item_hovered();
                ui.same_line();
                ui.help_marker(|| ui.text("Could not determine CM/LCM/NM mode"));
            }
        } else {
            ui.text(format!("{}", BossId::from_header_id(evtc.header.boss_id)));
            hovered = ui.is_item_hovered();
        };
        hovered
    }

    pub fn render_hovered(&self, ui: &Ui) {
        let Step::Done(evtc) = &self.evtc else {
            return;
        };
        ui.tooltip(|| {
            self.render_title(ui, evtc);
            if let Some(_table) = ui.begin_table(self.location.to_string_lossy(), 3) {
                for a in evtc.agents.iter().filter(|a| !a.account_name.is_empty()) {
                    ui.table_next_row();
                    ui.table_next_column();
                    if let Some(tex) = get_texture(identifier_from_agent(a)) {
                        Image::new(tex.id(), [16.0, 16.0]).build(ui);
                    }
                    ui.table_next_column();
                    ui.text(a.character_name.as_str());
                    ui.table_next_column();
                    ui.text(a.account_name.as_str());
                }
            }
        })
    }
}
