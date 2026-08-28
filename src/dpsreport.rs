use std::{
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;
use ureq::Response;
use ureq_multipart::MultipartRequest;

use crate::common::WorkerMessage;

pub struct DpsJob {
    pub index: usize,
    pub location: PathBuf,
    pub token: String,
}
thread_local! {
    static CLIENT: ureq::Agent = ureq::agent()
}

type DpsResult = Result<Result<DpsReportResponse, Instant>, anyhow::Error>;

const RETRY_DELAY: Duration = Duration::from_secs(30);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(300);

fn retry_in(delay: Duration) -> DpsResult {
    Ok(Err(Instant::now() + delay))
}

fn mask(message: &str, token: &str) -> String {
    if token.is_empty() {
        message.to_owned()
    } else {
        message.replace(token, "******")
    }
}

// Spacing dps.report asks for when it turns an upload away for rate limiting.
fn rate_limit_delay(error: &DpsReportError) -> Option<Duration> {
    if error.rate_limited != Some(true) {
        return None;
    }
    let per_minute = error.rate_per_minute.filter(|n| *n > 0)?;
    Some(Duration::from_secs((60 / per_minute).max(1) as u64).min(MAX_RETRY_DELAY))
}

fn retry_after(res: &Response) -> Option<Duration> {
    let secs: u64 = res.header("retry-after")?.trim().parse().ok()?;
    Some(Duration::from_secs(secs).min(MAX_RETRY_DELAY))
}

fn parse_report(body: &str) -> DpsResult {
    match serde_json::from_str::<DpsReportResponse>(body) {
        Ok(json) => Ok(Ok(json)),
        Err(e) => Err(anyhow::anyhow!("Error parsing json: {e}: {body}")),
    }
}

// Turn one upload attempt into a report, a retry, or a terminal error.
fn classify(result: Result<Response, ureq::Error>, token: &str) -> DpsResult {
    match result {
        Ok(res) => {
            log::info!("[DpsReport] Response: {}", mask(&format!("{res:?}"), token));
            if (200..300).contains(&res.status()) {
                parse_report(&res.into_string().unwrap_or_default())
            } else {
                Err(anyhow::anyhow!("Unknown Response Code: {}", res.status()))
            }
        }
        Err(ureq::Error::Status(status, res)) => match status {
            408 => retry_in(RETRY_DELAY),
            429 => retry_in(retry_after(&res).unwrap_or(RETRY_DELAY)),
            403 => check_json(&res.into_string().unwrap_or_default()),
            status if status >= 500 => retry_in(RETRY_DELAY),
            _ => Err(anyhow::anyhow!("Unknown error {status}")),
        },
        // Transport failure
        Err(e) => {
            log::warn!(
                "[DpsReport] {}",
                mask(&format!("Failed to upload file: {e}"), token)
            );
            retry_in(RETRY_DELAY)
        }
    }
}

fn check_json(body: &str) -> DpsResult {
    match serde_json::from_str::<Result<DpsReportResponse, DpsReportError>>(body) {
        Ok(json) => {
            match json {
                Ok(report) => Ok(Ok(report)), // somehow we got a valid report from an error response
                Err(e) => {
                    if e.error.contains("EI Failure")
                        || e.error.contains("An identical file was uploaded recently")
                        || e.error.contains("Encounter is too short")
                    {
                        Err(anyhow::anyhow!("Error 403: {}", e.error))
                    } else {
                        retry_in(rate_limit_delay(&e).unwrap_or(RETRY_DELAY))
                    }
                }
            }
        }
        Err(e) => Err(anyhow::anyhow!("Error parsing json: {e}: {body}")),
    }
}

pub fn run(inc: Receiver<DpsJob>, out: Sender<WorkerMessage>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("dpsreport-thread".to_string())
        .spawn(move || {
            for job in inc {
                let DpsJob {
                    index,
                    location,
                    token,
                } = job;
                log::info!("dpsreport for {:?}", location);
                let res = classify(upload_file(&location, &token), &token);
                if let Err(e) = out.send(WorkerMessage::dpsreport(index, res)) {
                    log::error!("[DpsReport] Failed to send dpsreport result to main thread: {e}");
                }
            }
        })
        .expect("Could not create dpsreport thread")
}

fn upload_file(location: &PathBuf, token: &str) -> Result<Response, ureq::Error> {
    log::info!("[DpsReport] Uploading {}", location.display());

    CLIENT.with(|c| {
        let mut req = c
            .post("https://dps.report/uploadContent")
            .query("json", "1");
        // Nothing reads a detailed WvW parse from here -- sessions have their
        // own from evtc.bel.st -- and asking for one is expensive enough to
        // 500 on long logs, so it is not asked for.
        //
        // log here so userToken is not shown in the log
        log::debug!("[DpsReport] Sending Request (+ usertoken, not shown) {req:?}");
        if !token.is_empty() {
            req = req.query("userToken", token);
        }
        req.send_multipart_file("file", location)
    })
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum PlayerResponse {
    #[allow(dead_code)]
    Seq(Vec<Player>),
    #[allow(dead_code)]
    Map(HashMap<String, Player>),
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DpsReportResponse {
    #[allow(dead_code)]
    pub id: String,
    pub permalink: String,
    pub user_token: String,
    pub encounter: Encounter,
    #[allow(dead_code)]
    pub players: PlayerResponse,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Encounter {
    pub boss_id: i64,
    pub success: bool,
    pub boss: String,
    pub is_cm: Option<bool>,
    pub is_legendary_cm: Option<bool>,
    pub emboldened: Option<i32>,
}

impl Encounter {
    pub fn format_mode(&self) -> Option<String> {
        // WvW logs don't have any other modes
        if self.boss_id == 1 {
            return Some("".into());
        }
        match (self.emboldened, self.is_cm, self.is_legendary_cm) {
            (_, _, Some(true)) => Some("LCM".into()),
            (_, Some(true), _) => Some("CM".into()),
            (Some(n @ 1..), _, _) => Some(format!("Emboldened {n})")),
            (Some(0), Some(false), Some(false)) => Some("".into()),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct Player {
    #[allow(dead_code)]
    pub display_name: String,
    #[allow(dead_code)]
    pub character_name: String,
    #[allow(dead_code)]
    pub profession: u32,
    #[allow(dead_code)]
    pub elite_spec: u32,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct DpsReportError {
    pub error: String,
    pub rate_limited: Option<bool>,
    pub rate_per_minute: Option<u32>,
}
