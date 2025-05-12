use std::fmt::Write;
use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, Sender},
    thread,
};

use revtc::{
    bossdata::{EliteSpec, Profession},
    evtc::Agent,
};

use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
};

use crate::common::WorkerMessage;

pub type EvtcJob = (usize, PathBuf);

pub fn run(inc: Receiver<EvtcJob>, out: Sender<WorkerMessage>) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name("uploader-evtc".to_string())
        .spawn(move || {
            loop {
                match inc.recv() {
                    Ok((index, path)) => {
                        unsafe {
                            if let Err(e) =
                                SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL)
                            {
                                log::error!(
                                    "[EVTC] Failed to set thread background priority ({}): {}",
                                    e.code(),
                                    e.message()
                                );
                            }
                        };
                        log::trace!("[EVTC] Processing {}", path.display());
                        let mut evtc = revtc::open(path);
                        if let Ok(e) = &mut evtc {
                            // Don't store cbtlog and skills for all the logs
                            e.shrink();
                        }

                        if let Err(e) = out.send(WorkerMessage::evtc(index, evtc)) {
                            log::error!("[EVTC] Failed to send evtc to main thread: {e}");
                        };
                        unsafe {
                            if let Err(e) =
                                SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_NORMAL)
                            {
                                log::error!(
                                    "[EVTC] Failed to unset thread background priority ({}): {}",
                                    e.code(),
                                    e.message()
                                );
                            }
                        };
                    }
                    Err(e) => {
                        log::trace!("[EVTC] Worker thread exiting: {e}");
                        break;
                    }
                }
            }
        })
        .unwrap()
}

pub fn identifier_from_agent(agent: &Agent) -> String {
    let mut ret = String::new();
    if agent.elite_spec != EliteSpec::Unknown {
        write!(ret, "{}", agent.elite_spec).ok();
    } else if agent.prof != Profession::Unknown {
        write!(ret, "{}", agent.prof).ok();
    }
    format!("UPLOADER_{}_16x16", ret.to_uppercase())
}
