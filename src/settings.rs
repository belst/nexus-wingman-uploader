use dirs_next::document_dir;
use std::{
    fs::{create_dir_all, File},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Settings {
    pub logpath: String,
    pub dpsreport_token: String,
}

impl Settings {
    fn default_dir() -> PathBuf {
        let mut base = document_dir().unwrap_or_default();
        base.push("Guild Wars 2");
        base.push("addons");
        base.push("arcdps");
        base.push("arcdps.cbtlogs");
        base
    }
    pub fn new() -> Self {
        Self {
            logpath: Self::default_dir().to_string_lossy().to_string(),
            dpsreport_token: String::new(),
        }
    }
    pub fn from_path<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let config = std::fs::read_to_string(path)?;

        Ok(serde_json::from_str(&config)?)
    }
    pub fn store<P: AsRef<Path>>(&self, path: P) -> anyhow::Result<()> {
        let path = path.as_ref();
        let prefix = path.parent().unwrap();
        create_dir_all(prefix)?;
        let mut file = File::options()
            .write(true)
            .append(false)
            .create(true)
            .truncate(true)
            .open(path)?;
        Ok(serde_json::to_writer_pretty(&mut file, self)?)
    }
}
