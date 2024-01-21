use serde::Deserialize;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{mpsc::Receiver, Arc, Mutex};
use std::thread::{spawn, JoinHandle};
use ureq::Agent;
use ureq_multipart::MultipartRequest;

use crate::log::{error, info, trace};
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
                let skip =
                    p.logtype == Logtype::Wvw || !unsafe { SETTINGS.get().unwrap().enable_wingman };
                drop(p);
                if !skip {
                    info("[DpsReportUploader] Uploading log".into());
                    let res = self.upload_download(file);
                    match res {
                        Ok((res, html, json, acc_name)) => {
                            self.set_token(Some(res.user_token));
                            let mut p = path.lock().unwrap();
                            p.dpsreporturl = Some(res.permalink);
                            p.html = html;
                            p.json = json;
                            p.acc_name = acc_name;
                            p.status = UploadStatus::DpsReportDone;
                        }
                        Err(e) => {
                            let mut p = path.lock().unwrap();
                            p.status = UploadStatus::Error;
                            error(format!("[DpsReportUploader] Error Uploading: {e}"));
                        }
                    }
                } else {
                    info("[DpsReportUploader] Uploading log, skipping Wingman".into());
                    let res = self.upload_file(file);
                    match res {
                        Ok(res) => {
                            self.set_token(Some(res.user_token));
                            let mut p = path.lock().unwrap();
                            p.dpsreporturl = Some(res.permalink);
                            p.status = UploadStatus::Done;
                        }
                        Err(e) => {
                            let mut p = path.lock().unwrap();
                            p.status = UploadStatus::Error;
                            error(format!("[DpsReportUploader] Error Uploading: {e}"));
                        }
                    }
                }
            }
        })
    }

    fn upload_download(
        &self,
        path: PathBuf,
    ) -> Result<(DpsReportResponse, Vec<u8>, Vec<u8>, Option<String>), anyhow::Error> {
        // we just assume nothing here breaks
        let upload = self.upload_file(path)?;
        trace(format!("[DpsReportUploader] Got response: {upload:?}"));
        let html = self.download_html(&upload.permalink)?;
        trace(format!(
            "[DpsReportUploader] Got html: {}...",
            String::from_utf8_lossy(&html[..20])
        ));
        let json = self.download_json(&upload.id)?;
        trace(format!(
            "[DpsReportUploader] Got json: {}...",
            String::from_utf8_lossy(&json[..20])
        ));
        let acc_name = Self::get_accname_from_json(&json);
        trace(format!("Account name: {acc_name:?}"));
        Ok((upload, html, json, acc_name))
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

    // Don't need to parse full json to get account name
    fn get_accname_from_json(bytes: &[u8]) -> Option<String> {
        // "recordedAccountBy":"belst.6815"
        let index = bytes
            .windows(b"\"recordedAccountBy\"".len())
            .position(|w| w == b"\"recordedAccountBy\"")?;

        // + 1 to skip `:`
        let mut bytes = &bytes[index + b"\"recordedAccountBy\"".len() + 1..];
        // Skip to the first `"` (should not do anything, unless json contains useless whitespace)
        while bytes[0] != b'"' {
            bytes = &bytes[1..];
        }
        bytes = &bytes[1..]; // skip `"`
        let end = bytes.iter().position(|&c| c == b'"')?;

        Some(String::from_utf8_lossy(&bytes[..end]).to_string())
    }

    fn download_json(&self, id: &str) -> Result<Vec<u8>, anyhow::Error> {
        let res = self
            .client
            .get("https://dps.report/getJson")
            .query("id", id)
            .call()?;
        let mut bytes = Vec::new();
        res.into_reader()
            // mostly wvw logs get that big
            .take(150 * 1024 * 1024) // unpacked log limit size, pretty big still, might need to test
            .read_to_end(&mut bytes)?;

        Ok(bytes)
    }

    // I believe that technically, this also contains the json from above, but too lazy too parse
    fn download_html(&self, permalink: &str) -> Result<Vec<u8>, anyhow::Error> {
        let res = self.client.get(permalink).call()?;
        let mut bytes = Vec::new();
        res.into_reader()
            .take(150 * 1024 * 1024)
            .read_to_end(&mut bytes)?;

        Ok(bytes)
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DpsReportResponse {
    id: String,
    permalink: String,
    user_token: String,
}
