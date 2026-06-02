use base64::{Engine, engine::general_purpose::STANDARD as B64};
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    thread,
};

use crate::common::WorkerMessage;

pub type DonBotJob = (usize, PathBuf, String, String); // index, file, base_url, bearer_token

thread_local! { static CLIENT: ureq::Agent = ureq::agent(); }

pub fn run(inc: Receiver<DonBotJob>, out: Sender<WorkerMessage>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("donbot-thread".into())
        .spawn(move || {
            for (index, location, base_url, token) in inc {
                let result = upload(&location, &base_url, &token);
                if let Err(e) = out.send(WorkerMessage::donbot(index, result)) {
                    log::error!("[DonBot] failed to send result: {e}");
                }
            }
        })
        .expect("could not create donbot thread")
}

fn upload(location: &PathBuf, base_url: &str, token: &str) -> anyhow::Result<(bool)> {
    let bytes = std::fs::read(location)?;
    let filename = location
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "upload.zevtc".into());

    let metadata = format!(
        "filename {},wingman {}",
        B64.encode(filename.as_bytes()),
        B64.encode(b"false")
    );

    // 1) Create
    let create = CLIENT.with(|c| {
        c.post(&format!("{}/api/upload/tus", base_url.trim_end_matches('/')))
            .set("Authorization", &format!("Bearer {token}"))
            .set("Tus-Resumable", "1.0.0")
            .set("Upload-Length", &bytes.len().to_string())
            .set("Upload-Metadata", &metadata)
            .call()
    })?;

    let location_header = create
        .header("Location")
        .ok_or_else(|| anyhow::anyhow!("tus create returned no Location header"))?
        .to_string();

    let patch_url = if location_header.starts_with("http") {
        location_header
    } else {
        format!("{}{}", base_url.trim_end_matches('/'), location_header)
    };

    // 2) Patch (single shot — zevtc files are small)
    CLIENT.with(|c| {
        c.patch(&patch_url)
            .set("Authorization", &format!("Bearer {token}"))
            .set("Tus-Resumable", "1.0.0")
            .set("Content-Type", "application/offset+octet-stream")
            .set("Upload-Offset", "0")
            .send_bytes(&bytes)
    })?;

    Ok(true)
}