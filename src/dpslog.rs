use std::{
    fmt::Display,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
};

use nexus::imgui::Ui;

use crate::{settings::Settings, ui::UiExt};

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
    pub dpsreporturl: Option<String>,
    pub wingmanurl: Option<String>,
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
        if let Err(e) = ui.link(
            self.dpsreporturl.as_deref().unwrap_or("Upload Pending"),
            self.dpsreporturl.as_ref(),
        ) {
            log::error!("Error opening browser: {e}");
        }
    }

    fn render_wingmanurl(&self, ui: &Ui) {
        let url = if !Settings::get().enable_wingman && self.wingmanurl.is_none() {
            "Wingman uploads disabled"
        } else if self.wingmanurl.is_none() {
            "Wingmanupload pending"
        } else {
            self.wingmanurl.as_ref().unwrap()
        };
        if let Err(e) = ui.link(url, self.wingmanurl.as_ref()) {
            log::error!("Error opening browser: {e}");
        }
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
            if ui.button("Retry") {
                match e {
                    ErrorKind::Wingman(_) => self.status = UploadStatus::DpsReportDone,
                    ErrorKind::DpsReport(_) => self.status = UploadStatus::Pending,
                }
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

        ui.table_next_column();
        self.render_wingmanurl(ui);

        ui.table_next_column();
        self.render_retry(ui);
    }
}
