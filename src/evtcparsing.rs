use std::{
    sync::{mpsc::Receiver, Arc, Mutex},
    thread::{spawn, JoinHandle},
};

use crate::{
    dpslog::{self, ErrorKind, UploadStatus},
    Upload,
};

pub fn run(rx: Receiver<Arc<Mutex<Upload>>>) -> JoinHandle<()> {
    spawn(move || {
        while let Ok(path) = rx.recv() {
            let mut p = path.lock().unwrap();
            p.status = UploadStatus::Parsing;
            let file = p.file.clone();
            drop(p);

            log::info!("[EvtcParser] Parsing log");
            match revtc::open(file) {
                Ok(encounter) => {
                    let mut p = path.lock().unwrap();
                    p.status = UploadStatus::ParsingDone;
                    p.logtype = if encounter.header.boss_id == 1 {
                        dpslog::Logtype::Wvw
                    } else {
                        dpslog::Logtype::Pve
                    };
                }
                Err(e) => {
                    let mut p = path.lock().unwrap();
                    log::error!("[EvtcParser] Error Parsing: {e}");
                    p.status = UploadStatus::Error(ErrorKind::Parser(e));
                }
            };
        }
    })
}
