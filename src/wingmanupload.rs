use std::{
    sync::{mpsc::Receiver, Arc, Mutex},
    thread::{spawn, JoinHandle},
};

use ureq::Agent;

use crate::{agent, Upload, UploadStatus};

pub struct WingmanUploader {
    client: Agent,
}

impl WingmanUploader {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { client: agent() })
    }

    fn upload(&self, url: &str) -> anyhow::Result<()> {
        let response = self
            .client
            .get("https://gw2wingman.nevermindcreations.de/manualUploadOne")
            .query("link", url)
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
                p.status = UploadStatus::WingmanInProgress;
                let url = p.dpsreporturl.clone().unwrap();
                drop(p);
                let res = self.upload(&url);
                let mut p = path.lock().unwrap();
                if let Err(e) = res {
                    log::error!("[WingmanUploader] {e}");
                    p.status = UploadStatus::Error(crate::dpslog::ErrorKind::Wingman(e));
                } else {
                    p.wingmanurl = Some(
                        String::from("https://gw2wingman.nevermindcreations.de/log/")
                            + url.split('/').last().unwrap(),
                    );
                    p.status = UploadStatus::Done;
                }
            }
        })
    }
}
