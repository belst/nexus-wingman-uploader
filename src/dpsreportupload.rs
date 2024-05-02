use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{mpsc::Receiver, Arc, Mutex};
use std::thread::{spawn, JoinHandle};
use ureq::Agent;
use ureq_multipart::MultipartRequest;

use crate::settings::Settings;
use crate::{agent, dpslog::Logtype, Upload, UploadStatus};

#[derive(Debug)]
pub struct DpsReportUploader {
    session_token: Mutex<Option<String>>,
    client: Agent,
}

impl DpsReportUploader {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            session_token: Mutex::new(None),
            client: agent(),
        })
    }
    pub fn with_token(token: String) -> Arc<Self> {
        Arc::new(Self {
            session_token: Mutex::new(Some(token)),
            client: agent(),
        })
    }
    pub fn set_token(&self, token: Option<String>) {
        if let Some(ref token) = token {
            Settings::get().set_token(token.clone());
        }
        *self.session_token.lock().unwrap() = token;
    }
    pub fn token(&self) -> Option<String> {
        self.session_token.lock().unwrap().clone()
    }

    pub fn run(self: Arc<Self>, rx: Receiver<Arc<Mutex<Upload>>>) -> JoinHandle<()> {
        spawn(move || {
            while let Ok(path) = rx.recv() {
                let mut p = path.lock().unwrap();
                p.status = UploadStatus::DpsReportInProgress;
                let file = p.file.clone();
                drop(p);

                log::info!("[DpsReportUploader] Uploading log");
                let res = self.upload_file(file);
                match res {
                    Ok(res) => {
                        self.set_token(Some(res.user_token));
                        let mut p = path.lock().unwrap();
                        p.dpsreporturl = Some(res.permalink);
                        p.status = UploadStatus::DpsReportDone;
                        p.logtype = if res.encounter.boss_id == 1 {
                            Logtype::Wvw
                        } else {
                            Logtype::Pve
                        };
                    }
                    Err(e) => {
                        let mut p = path.lock().unwrap();
                        log::error!("[DpsReportUploader] Error Uploading: {e}");
                        p.status = UploadStatus::Error(crate::dpslog::ErrorKind::DpsReport(e));
                    }
                }
            }
        })
    }

    fn upload_file(&self, path: PathBuf) -> Result<DpsReportResponse, anyhow::Error> {
        let mut req = self
            .client
            .post("https://dps.report/uploadContent")
            .query("json", "1");
        if let Some(t) = self.token() {
            req = req.query("userToken", &t)
        }
        Ok(req.send_multipart_file("file", path)?.into_json()?)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DpsReportResponse {
    permalink: String,
    user_token: String,
    encounter: Encounter,
    // players: Vec<Player>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Encounter {
    boss_id: i64,
    success: bool,
    boss: String,
    is_cm: bool,
}

#[derive(Debug, Deserialize)]
struct Player {
    display_name: String,
    character_name: String,
    profession: u32,
    elite_spec: u32,
}
