use std::{
    io::Read,
    path::Path,
    sync::{mpsc::Receiver, Arc, Mutex},
    thread::{spawn, JoinHandle},
};

use ureq::Agent;
use ureq_multipart::MultipartBuilder;

use crate::{
    agent,
    log::{error, trace},
    Upload, UploadStatus,
};

pub struct WingmanUploader {
    client: Agent,
}

impl WingmanUploader {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { client: agent() })
    }

    fn upload_processed<P: AsRef<Path>>(
        &self,
        name: String,
        path: P,
        html: Vec<u8>,
        json: Vec<u8>,
        acc_name: Option<String>,
    ) -> anyhow::Result<()> {
        let jsonfilename = name.clone() + ".json";
        let htmlfilename = name + ".html";
        let mut multipart = MultipartBuilder::new()
            .add_stream(
                &mut json.as_slice(),
                "jsonfile",
                Some(&jsonfilename),
                Some("application/json".parse()?),
            )?
            .add_stream(
                &mut html.as_slice(),
                "htmlfile",
                Some(&htmlfilename),
                Some("text/html".parse()?),
            )?
            .add_file("file", &path)?;

        if let Some(acc_name) = acc_name {
            multipart = multipart.add_text("account", &acc_name)?;
        }
        let (content_type, data) = multipart.finish()?;
        trace(format!(
            "[WingmanUploader] Uploading to wingman: {}",
            path.as_ref().display()
        ));
        let response = self
            .client
            .post("https://gw2wingman.nevermindcreations.de/uploadProcessed")
            .set("Content-Type", &content_type)
            .send_bytes(&data)?;

        let mut s = String::new();
        response.into_reader().take(5).read_to_string(&mut s)?;

        trace(format!("[WingmanUploader] Response: {s}[...]"));

        if s.to_lowercase() == "false" {
            anyhow::bail!("Wingman Error");
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
                let html = std::mem::take(&mut p.html);
                let json = std::mem::take(&mut p.json);
                let filepath = p.file.clone();

                let name = filepath
                    .file_stem()
                    .expect("Filename to exist")
                    .to_string_lossy()
                    .to_string();
                let acc_name = p.acc_name.clone();
                drop(p);
                let res = self.upload_processed(name, filepath, html, json, acc_name);
                // let res = self.upload(url);
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
