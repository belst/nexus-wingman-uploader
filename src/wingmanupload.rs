use std::{
    sync::{mpsc::Receiver, Arc, Mutex},
    thread::{spawn, JoinHandle},
};

use serde::Deserialize;
use ureq::Agent;

use crate::{agent, Upload, UploadStatus};

pub struct WingmanUploader {
    client: Agent,
}

#[derive(Deserialize, Debug)]
struct LogQueueResponse {
    link: String,
    note: String,
    success: i32,
}

impl WingmanUploader {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { client: agent() })
    }

    fn upload(&self, url: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .get("https://gw2wingman.nevermindcreations.de/api/importLogQueued")
            .query("link", url)
            .call()?
            .into_json()
            .map(|r: LogQueueResponse| r.success != 0)
            .map_err(|e| log::trace!("[WingmanUploader] Error parsing response: {e}"))
            .unwrap_or(false);

        if !response {
            anyhow::bail!("Response does not contain ✔️");
        }

        Ok(())
    }

    /// Experimental
    fn upload_evtc(&self, file: impl AsRef<std::path::Path>) -> anyhow::Result<()> {
        #[derive(Deserialize, Debug)]
        struct UploadEvtcResponse {
            result: bool,
        }
        let file = file.as_ref();
        let parsed = revtc::open(&file)?;
        let acc_name = parsed.pov.map(|a| a.account_name);
        let builder = ureq_multipart::MultipartBuilder::new()
            .add_text("account", acc_name.unwrap_or("".to_string()).as_str())?
            .add_text(
                "filesize",
                std::fs::metadata(file)?.len().to_string().as_str(),
            )?
            .add_text("triggerID", format!("{}", parsed.header.boss_id).as_str())?
            .add_file("file", file)?;
        let (content_type, data) = builder.finish()?;
        let _response = self
            .client
            // .post("https://gw2wingman.nevermindcreations.de/uploadEVTC")
            .post("http://new.bel.st:3334/evtc")
            .set("Content-Type", &content_type)
            .send_bytes(data.as_slice())?
            .into_json()
            .map(|r: UploadEvtcResponse| r.result)
            .map_err(|e| log::trace!("[WingmanUploader] Error parsing response: {e}"))
            .unwrap_or(true);

        Ok(())
    }

    pub fn run(self: Arc<Self>, rx: Receiver<Arc<Mutex<Upload>>>) -> JoinHandle<()> {
        spawn(move || {
            while let Ok(path) = rx.recv() {
                let mut p = path.lock().unwrap();
                p.status = UploadStatus::WingmanInProgress;
                let file = p.file.clone();
                drop(p);
                let res = self.upload_evtc(&file);
                let mut p = path.lock().unwrap();
                if let Err(e) = res {
                    log::error!("[WingmanUploader] {e}");
                    p.status = UploadStatus::Error(crate::dpslog::ErrorKind::Wingman(e));
                } else {
                    p.wingmanurl = Some("unknown".into());
                    p.status = UploadStatus::Done;
                }
            }
        })
    }
}
