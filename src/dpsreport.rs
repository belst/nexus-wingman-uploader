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

pub type DpsJob = (usize, PathBuf, String);
thread_local! {
    static CLIENT: ureq::Agent = ureq::agent()
}

fn check_json(body: &str) -> Result<Result<DpsReportResponse, Instant>, anyhow::Error> {
    match serde_json::from_str::<Result<DpsReportResponse, DpsReportError>>(&body) {
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
                        // Generic forbidden. we retry in 30 seconds
                        Ok(Err(Instant::now() + Duration::from_secs(30)))
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
            for (index, location, token) in inc {
                log::info!("dpsreport for {:?}", location);
                let res = match upload_file(location, &token) {
                    Err(ureq::Error::Status(status, res)) => match status {
                        408 | 429 => Ok(Err(Instant::now() + Duration::from_secs(30))),
                        status if status >= 500 => {
                            Ok(Err(Instant::now() + Duration::from_secs(30)))
                        }
                        403 => {
                            let body = res.into_string().unwrap_or_default();
                            check_json(&body)
                        }
                        _ => Err(anyhow::anyhow!("Unknown error {}", res.status())),
                    },
                    Err(e) => {
                        let msg = format!("Failed to upload file: {e}").replace(&token, "******");
                        log::error!("[DpsReport] {msg}");
                        Err(anyhow::anyhow!(msg))
                    }
                    Ok(res) => {
                        log::info!(
                            "[DpsReport] Response: {}",
                            format!("{res:?}").replace(&token, "******")
                        );

                        if (200..300).contains(&res.status()) {
                            let body = res.into_string().unwrap_or_default();
                            match serde_json::from_str::<DpsReportResponse>(&body) {
                                Ok(json) => Ok(Ok(json)),
                                Err(e) => Err(anyhow::anyhow!("Error parsing json: {e}: {body}")),
                            }
                        } else {
                            Err(anyhow::anyhow!("Unknown Response Code: {}", res.status()))
                        }
                    }
                };
                if let Err(e) = out.send(WorkerMessage::dpsreport(index, res)) {
                    log::error!("[DpsReport] Failed to send dpsreport result to main thread: {e}");
                }
            }
        })
        .expect("Could not create dpsreport thread")
}

fn upload_file(location: PathBuf, token: &str) -> Result<Response, ureq::Error> {
    log::info!("[DpsReport] Uploading {}", location.display());

    CLIENT.with(|c| {
        let mut req = c
            .post("https://dps.report/uploadContent")
            .query("json", "1");
        if !token.is_empty() {
            req = req.query("userToken", token);
        }
        req.send_multipart_file("file", &location)
    })
}

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum PlayerResponse {
    Seq(Vec<Player>),
    Map(HashMap<String, Player>),
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DpsReportResponse {
    pub id: String,
    pub permalink: String,
    pub user_token: String,
    pub encounter: Encounter,
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
    pub display_name: String,
    pub character_name: String,
    pub profession: u32,
    pub elite_spec: u32,
}

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct DpsReportError {
    pub error: String,
    pub rate_limited: Option<bool>,
    pub rate_per_minute: Option<u32>,
}
