use anyhow::Result;
use revtc::evtc::Encounter;

use crate::dpsreport::DpsReportResponse;
use crate::util::e;

pub const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
pub const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

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
}

#[derive(Debug)]
pub enum WorkerType {
    DpsReport(Result<Result<DpsReportResponse, std::time::Instant>>),
    Wingman(WingmanUpdate),
    Aleeva(Result<bool>),
    Evtc(Result<Encounter>),
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
