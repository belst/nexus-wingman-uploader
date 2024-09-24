use serde::Deserialize;
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    thread,
};

use crate::common::WorkerMessage;

pub type WingmanJob = (usize, PathBuf, String, u16);

#[derive(Debug, Deserialize)]
struct EvtcResponse {
    result: bool,
}

thread_local! {
    static CLIENT: ureq::Agent = ureq::agent()
}

pub fn run(inc: Receiver<WingmanJob>, out: Sender<WorkerMessage>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for (index, location, account_name, boss_id) in inc {
            let result = upload(location, account_name, boss_id);
            if let Err(e) = out.send(WorkerMessage::wingman(index, result)) {
                log::error!("[Wingman] Failed to send wingman result to main thread: {e}");
            }
        }
    })
}

fn upload(location: PathBuf, account_name: String, boss_id: u16) -> anyhow::Result<bool> {
    log::info!("[Wingman] Uploading {}", location.display());

    let builder = ureq_multipart::MultipartBuilder::new()
        .add_text("account", account_name.as_str())?
        .add_text(
            "filesize",
            std::fs::metadata(&location)?.len().to_string().as_str(),
        )?
        .add_text("triggerID", format!("{}", boss_id).as_str())?
        .add_file("file", location)?;
    let (content_type, data) = builder.finish()?;
    let response = CLIENT.with(|c| {
        let resp = c
            // .post("https://gw2wingman.nevermindcreations.de/uploadEVTC")
            .post("https://evtc.bel.st/evtc")
            .set("Content-Type", &content_type)
            .send_bytes(data.as_slice())?;
        if resp.status() == 409 {
            // rejected because it's a duplicate
            // just assume it's ok
            Ok(true)
        } else {
            Ok(resp.into_json().map(|r: EvtcResponse| r.result)?)
        }
    });
    response
}
