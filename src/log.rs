use nexus_rs::raw_structs::ELogLevel;

use crate::API;

pub fn log(level: ELogLevel, msg: String) {
    unsafe {
        let api = API.assume_init();
        (api.log)(level, (msg + "\0").as_ptr() as _);
    }
}

pub fn info(msg: String) {
    log(ELogLevel::INFO, msg);
}

pub fn error(msg: String) {
    log(ELogLevel::CRITICAL, msg);
}
