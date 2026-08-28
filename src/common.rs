use std::path::Path;

use anyhow::{Result, anyhow};
use revtc::evtc::Encounter;

use crate::dpsreport::DpsReportResponse;
use crate::util::e;

pub const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
pub const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

pub const EVTC_API_BASE: &str = "https://evtc.bel.st";

pub fn evtc_multipart(
    location: &Path,
    account: &str,
    boss_id: u16,
) -> Result<(String, Vec<u8>, String)> {
    let filesize = std::fs::metadata(location)?.len().to_string();

    let (content_type, data) = ureq_multipart::MultipartBuilder::new()
        .add_text("account", account)?
        .add_text("filesize", &filesize)?
        .add_text("triggerID", &boss_id.to_string())?
        .add_file("file", location)?
        .finish()?;

    Ok((content_type, data, filesize))
}

pub fn file_name(location: &Path) -> Result<String> {
    Ok(location
        .file_name()
        .ok_or_else(|| anyhow!("log path has no file name"))?
        .to_string_lossy()
        .into_owned())
}

#[derive(Debug)]
pub struct WorkerMessage {
    pub index: usize,
    pub payload: WorkerType,
}

impl WorkerMessage {
    pub fn evtc(index: usize, evtc: Result<Encounter>) -> Self {
        Self {
            index,
            payload: WorkerType::Evtc(evtc),
        }
    }

    pub fn dpsreport(
        index: usize,
        dpsreport: Result<Result<DpsReportResponse, std::time::Instant>>,
    ) -> WorkerMessage {
        WorkerMessage {
            index,
            payload: WorkerType::DpsReport(dpsreport),
        }
    }
    pub fn wingman(index: usize, wingman: WingmanUpdate) -> WorkerMessage {
        WorkerMessage {
            index,
            payload: WorkerType::Wingman(wingman),
        }
    }

    pub fn aleeva(index: usize, aleeva: Result<bool>) -> WorkerMessage {
        WorkerMessage {
            index,
            payload: WorkerType::Aleeva(aleeva),
        }
    }

    pub fn session(index: usize, session: Result<bool>) -> WorkerMessage {
        WorkerMessage {
            index,
            payload: WorkerType::Session(session),
        }
    }

    pub fn wvw_upload(index: usize, update: Result<WvwUploadUpdate>) -> WorkerMessage {
        WorkerMessage {
            index,
            payload: WorkerType::WvwUpload(update),
        }
    }
}

#[derive(Debug)]
pub enum WorkerType {
    DpsReport(Result<Result<DpsReportResponse, std::time::Instant>>),
    Wingman(WingmanUpdate),
    Aleeva(Result<bool>),
    Session(Result<bool>),
    Evtc(Result<Encounter>),
    /// The id a WvW upload came back with, and how the parse behind it goes.
    WvwUpload(Result<WvwUploadUpdate>),
}

/// Where a WvW log stands between being accepted and having a report.
///
/// The parse can run for fifteen minutes, so the id alone says nothing about
/// whether there is anything to open yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WvwParse {
    Queued {
        position: usize,
    },
    Parsing,
    Parsed,
    /// The parse failed, so this log will never appear in a report.
    Failed(String),
}

impl WvwParse {
    pub fn label(&self) -> String {
        match self {
            Self::Queued { position: 0 } => e("Queued for parsing"),
            Self::Queued { position } => format!("{} ({})", e("Queued for parsing"), position),
            Self::Parsing => e("Parsing..."),
            Self::Parsed => e("Parsed for the session"),
            Self::Failed(message) => e("The parse failed: ") + message,
        }
    }
}

#[derive(Debug)]
pub enum WvwUploadUpdate {
    /// The id the log is addressed by from here on. The session step only needs
    /// this much: wvwsessions waits out the parse itself.
    Uploaded(String),
    /// How the parse behind that id is going, for the tooltip.
    Parse(WvwParse),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WingmanProgress {
    Uploading,
    Queued { position: usize },
    Processing,
    // ERR_CAPACITY_CRITICAL Classic
    Deferred { attempt: u32 },
    Checking,
}

impl WingmanProgress {
    pub fn label(&self) -> String {
        match self {
            Self::Uploading => e("Uploading..."),
            Self::Queued { position: 0 } => e("Queued"),
            Self::Queued { position } => format!("{} ({})", e("Queued"), position),
            Self::Processing => e("Parsing..."),
            Self::Deferred { attempt } => {
                format!("{} ({})", e("Wingman is busy, retrying"), attempt)
            }
            Self::Checking => e("Waiting for wingman..."),
        }
    }
}

#[derive(Debug)]
pub enum WingmanUpdate {
    Progress(WingmanProgress),
    // None = /checkUploadSuccessfulWithLog didn't give us a link after 5 minutes.
    Done(Option<String>),
    Skipped,
    Error(anyhow::Error),
}
