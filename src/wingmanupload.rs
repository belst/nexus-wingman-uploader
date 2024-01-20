use std::{
    sync::{mpsc::Receiver, Arc, Mutex},
    thread::{spawn, JoinHandle},
};

use ureq::Agent;

use crate::{agent, log::error, Upload, UploadStatus};

pub struct WingmanUploader {
    client: Agent,
}

impl WingmanUploader {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { client: agent() })
    }

    fn upload(&self, url: String) -> anyhow::Result<()> {
        let response = self
            .client
            .get("https://gw2wingman.nevermindcreations.de/manualUploadOne")
            .query("link", &url)
            .call()?;

        if !response
            .into_string()
            .map(|s| s.contains("✔️"))
            .unwrap_or(false)
        {
            anyhow::bail!("Response does not contain ✔️");
        }

        Ok(())
    }

    pub fn run(self: Arc<Self>, rx: Receiver<Arc<Mutex<Upload>>>) -> JoinHandle<()> {
        spawn(move || {
            while let Ok(path) = rx.recv() {
                let mut p = path.lock().unwrap();
                if p.status == UploadStatus::Quit {
                    break;
                }
                p.status = UploadStatus::WingmanInProgress;
                let url = p.dpsreporturl.as_ref().unwrap().clone();
                drop(p);
                let res = self.upload(url);
                let mut p = path.lock().unwrap();
                if let Err(e) = res {
                    error(format!("[WingmanUploader] {e}"));
                    p.status = UploadStatus::Error;
                } else {
                    p.status = UploadStatus::Done;
                }
            }
        })
    }
}
