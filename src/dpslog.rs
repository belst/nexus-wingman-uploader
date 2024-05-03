use std::{
    fmt::Display,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

use nexus::{
    imgui::{Image, ImageButton, Ui},
    texture::get_texture,
};

use crate::{dpsreportupload::DpsReportResponse, ui::UiExt};

pub type UploadRef = Arc<Mutex<Upload>>;

pub static mut UPLOADS: OnceLock<Vec<UploadRef>> = OnceLock::new();

#[derive(Debug)]
pub enum ErrorKind {
    Wingman(anyhow::Error),
    DpsReport(anyhow::Error),
}

impl Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorKind::DpsReport(e) => write!(f, "[DpsReport] Error: {e}"),
            ErrorKind::Wingman(e) => write!(f, "[Wingman] Error: {e}"),
        }
    }
}

#[derive(Debug, Default)]
pub enum UploadStatus {
    #[default]
    Pending,
    DpsReportInProgress,
    DpsReportDone,
    WingmanInProgress,
    WingmanSkipped,
    Done,
    Error(ErrorKind),
}

impl Display for UploadStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UploadStatus::Pending => f.write_str("Pending"),
            UploadStatus::DpsReportInProgress => f.write_str("Uploading to dps.report"),
            UploadStatus::DpsReportDone => f.write_str("Finished dps.report upload"),
            UploadStatus::WingmanInProgress => f.write_str("Adding to wingman Queue"),
            UploadStatus::WingmanSkipped => f.write_str("Wingman disabled or WvW Log"),
            UploadStatus::Done => f.write_str("All done"),
            UploadStatus::Error(ErrorKind::Wingman(_)) => f.write_str("Wingman Error"),
            UploadStatus::Error(ErrorKind::DpsReport(_)) => f.write_str("DpsReport Error"),
        }
    }
}

#[derive(Debug, PartialEq, Default)]
pub enum Logtype {
    #[default]
    Pve,
    Wvw,
}

#[derive(Debug, Default)]
pub struct Upload {
    pub status: UploadStatus,
    pub logtype: Logtype,
    pub file: PathBuf,
    pub dpsreportobject: Option<DpsReportResponse>,
    pub dpsreporturl: Option<String>,
    pub wingmanurl: Option<String>,
}

// Icon pulsation speed
const PULSE_SPEED: f32 = 5.0;

fn pulse(t: f32) -> f32 {
    (1.0 + t.sin()) / 2.0
}

impl Upload {
    fn basename(&self) -> String {
        let file = self
            .file
            .iter()
            .rev()
            .take(2)
            .fold(String::new(), |acc, c| {
                c.to_string_lossy().to_string() + "\\" + &acc
            });
        file.trim_end_matches('\\').to_string()
    }

    fn render_dpsreuprurl(&self, ui: &Ui) {
        static mut TS: Option<Instant> = None;

        // safety: this only gets called in the render thread
        unsafe {
            if TS.is_none() {
                TS = Some(Instant::now());
            }
        }
        let ts = unsafe { TS.unwrap() };
        let Some(text) = get_texture("DPSREPORT_LOGO") else {
            return;
        };
        if let Some(url) = self.dpsreporturl.as_ref() {
            let push_id = ui.push_id(&(self.file.to_string_lossy() + "btn_dpsreport"));
            if ImageButton::new(text.id(), [16.0, 16.0])
                .frame_padding(0)
                .build(ui)
            {
                //open url
                if let Err(e) = open::that_detached(url) {
                    log::error!("Error opening browser: {e}");
                }
            }
            push_id.end();
            if ui.is_item_hovered() {
                ui.tooltip_text("Open log in Browser");
            }
        } else {
            Image::new(text.id(), [16.0, 16.0])
                .tint_col([
                    1.0,
                    1.0,
                    1.0,
                    pulse(PULSE_SPEED * ts.elapsed().as_secs_f32()),
                ])
                .build(ui)
        }
    }

    fn render_wingmanurl(&self, ui: &Ui) {
        static mut TS: Option<Instant> = None;

        // safety: this only gets called in the render thread
        unsafe {
            if TS.is_none() {
                TS = Some(Instant::now());
            }
        }
        let ts = unsafe { TS.unwrap() };
        let Some(text) = get_texture("WINGMAN_LOGO") else {
            return;
        };
        if let UploadStatus::WingmanSkipped = self.status {
            Image::new(text.id(), [16.0, 16.0])
                .tint_col([1.0, 1.0, 1.0, 0.5])
                .build(ui);
        } else if self.wingmanurl.is_none() {
            Image::new(text.id(), [16.0, 16.0])
                .tint_col([
                    1.0,
                    1.0,
                    1.0,
                    pulse(ts.elapsed().as_secs_f32() * PULSE_SPEED),
                ])
                .build(ui);
        } else {
            let push_id = ui.push_id(&(self.file.to_string_lossy() + "btn_wingman"));
            if ImageButton::new(text.id(), [16.0, 16.0])
                .frame_padding(0)
                .build(ui)
            {
                //open url
                if let Err(e) = open::that_detached(self.wingmanurl.as_ref().unwrap()) {
                    log::error!("Error opening browser: {e}");
                }
            }
            push_id.end();
            if ui.is_item_hovered() {
                ui.tooltip_text("Open wingman in Browser");
            }
        };
    }

    fn render_status(&self, ui: &Ui) {
        ui.text(format!("{}", self.status));
        if let UploadStatus::Error(ref e) = self.status {
            if ui.is_item_hovered() {
                ui.tooltip_text(format!("{e}"));
            }
        }
    }

    fn render_retry(&mut self, ui: &Ui) {
        if let UploadStatus::Error(ref e) = self.status {
            // this only gets loaded once u open the addons panel so need a custom icon
            let Some(text) = get_texture("TEX_BTNREFRESH") else {
                return;
            };
            let push_id = ui.push_id(&(self.file.to_string_lossy() + "btn_retry"));
            if ImageButton::new(text.id(), [16.0, 16.0])
                .frame_padding(0)
                .build(ui)
            {
                match e {
                    ErrorKind::Wingman(_) => self.status = UploadStatus::DpsReportDone,
                    ErrorKind::DpsReport(_) => self.status = UploadStatus::Pending,
                }
            }
            push_id.end();
            if ui.is_item_hovered() {
                ui.tooltip_text("Retry");
            }
        }
    }

    pub fn render_row(&mut self, ui: &Ui) {
        ui.table_next_column();
        self.render_status(ui);

        ui.table_next_column();
        ui.text(self.basename());

        ui.table_next_column();
        self.render_dpsreuprurl(ui);
        ui.same_line();
        self.render_wingmanurl(ui);
        ui.same_line();
        self.render_retry(ui);
    }
}
