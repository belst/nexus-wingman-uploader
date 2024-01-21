use serde::Deserialize;
use std::path::PathBuf;
use std::sync::{mpsc::Receiver, Arc, Mutex};
use std::thread::{spawn, JoinHandle};
use ureq::Agent;
use ureq_multipart::MultipartRequest;

use crate::log::{error, info};
use crate::{agent, Logtype, Upload, UploadStatus, SETTINGS};

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
            unsafe {
                SETTINGS.get_mut().unwrap().dpsreport_token = token.clone();
            }
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
                if p.status == UploadStatus::Quit {
                    break;
                }
                p.status = UploadStatus::DpsReportInProgress;
                let file = p.file.clone();
                drop(p);

                info("[DpsReportUploader] Uploading log".into());
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
                        p.status = UploadStatus::Error;
                        error(format!("[DpsReportUploader] Error Uploading: {e}"));
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Encounter {
    boss_id: i64,
}
