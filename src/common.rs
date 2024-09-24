use anyhow::Result;
use revtc::evtc::Encounter;

use crate::dpsreport::DpsReportResponse;

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
    // should be a url later instead of bool
    pub fn wingman(index: usize, wingman: Result<bool>) -> WorkerMessage {
        WorkerMessage {
            index,
            payload: WorkerType::Wingman(wingman),
        }
    }
}

#[derive(Debug)]
pub enum WorkerType {
    DpsReport(Result<Result<DpsReportResponse, std::time::Instant>>),
    Wingman(Result<bool>),
    Evtc(Result<Encounter>),
}
