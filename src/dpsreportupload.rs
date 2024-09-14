use nexus::imgui::Ui;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc::Receiver, Arc, Mutex};
use std::thread::{spawn, JoinHandle};
use ureq::{Agent, Response};
use ureq_multipart::MultipartRequest;

use crate::e;
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
                        let r = res.clone();
                        self.set_token(Some(res.user_token));
                        let mut p = path.lock().unwrap();
                        p.dpsreportobject = Some(r);
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
                        use crate::error::Error as E;
                        use ureq::Error as UreqError;
                        let error = E::Unknown(anyhow::anyhow!("{e}"));
                        match e {
                            E::Ureq(UreqError::Status(_, body)) => {
                                log::error!(
                                    "[DpsReportUploader] Body: {:?}",
                                    body.into_json::<serde_json::Value>()
                                );
                            }
                            _ => {}
                        }
                        p.status = UploadStatus::Error(crate::dpslog::ErrorKind::DpsReport(error));
                    }
                }
            }
        })
    }

    fn upload_file(&self, path: PathBuf) -> Result<DpsReportResponse, crate::error::Error> {
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

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
enum PlayerResponse {
    Seq(Vec<Player>),
    Map(HashMap<String, Player>),
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DpsReportResponse {
    id: String,
    permalink: String,
    user_token: String,
    encounter: Encounter,
    players: PlayerResponse,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Encounter {
    boss_id: i64,
    success: bool,
    boss: String,
    is_cm: bool,
}

#[derive(Debug, Deserialize, Clone)]
struct Player {
    display_name: String,
    character_name: String,
    profession: u32,
    elite_spec: u32,
}

impl DpsReportResponse {
    fn boss_title(&self) -> String {
        if self.encounter.is_cm {
            format!("{} (CM)", self.encounter.boss)
        } else {
            self.encounter.boss.clone()
        }
    }
    fn success(&self) -> String {
        if self.encounter.success {
            "Success".into()
        } else {
            "Failed".into()
        }
    }
    pub fn render(&self, ui: &Ui) {
        ui.tooltip(|| {
            ui.text(self.boss_title());
            ui.text_colored(
                [
                    !self.encounter.success as i32 as f32,
                    self.encounter.success as i32 as f32,
                    0.0,
                    1.0,
                ],
                e(format!("Status: {}", self.success()).as_str()),
            );
            if let Some(_table) = ui.begin_table(self.id.as_str(), 2) {
                let it = match &self.players {
                    PlayerResponse::Seq(ref v) => {
                        Box::new(v.iter()) as Box<dyn Iterator<Item = &Player>>
                    }
                    PlayerResponse::Map(ref m) => Box::new(m.values()),
                };
                for p in it {
                    ui.table_next_row();
                    ui.table_next_column();
                    ui.text(p.character_name.as_str());
                    ui.table_next_column();
                    ui.text(p.display_name.as_str());
                }
            }
        });
    }
}
