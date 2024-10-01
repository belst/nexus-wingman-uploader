use std::{
    path::PathBuf,
    sync::mpsc::{Receiver, TryRecvError},
};

use notify::{
    event::{ModifyKind, RenameMode},
    Error, ErrorKind, Event, EventKind,
};

pub trait ReceiverExt {
    fn next_log(&self) -> Result<Box<dyn Iterator<Item = PathBuf>>, Error>;
}

impl ReceiverExt for Receiver<Result<Event, Error>> {
    fn next_log(&self) -> Result<Box<dyn Iterator<Item = PathBuf>>, Error> {
        match self.try_recv() {
            Ok(Ok(event)) => {
                if EventKind::Modify(ModifyKind::Name(RenameMode::To)) == event.kind {
                    return Ok(Box::new(
                        event.paths.into_iter().filter(|p| p.is_file()).filter(|p| {
                            p.extension().is_some_and(|e| {
                                ["evtc", "zevtc"].contains(&e.to_string_lossy().as_ref())
                            })
                        }),
                    ));
                } else {
                    Err(Error::new(ErrorKind::Generic("Not a logfile".to_string())))
                }
            }
            Ok(Err(e)) => Err(e),
            Err(TryRecvError::Empty) => {
                Err(Error::new(ErrorKind::Generic("Empty queue".to_string())))
            }
            Err(TryRecvError::Disconnected) => {
                Err(Error::new(ErrorKind::Generic("Disconnected".to_string())))
            }
        }
    }
}
