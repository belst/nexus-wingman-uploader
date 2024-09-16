#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Http Error: {0}")]
    Ureq(#[from] ureq::Error),
    #[error("Io Error: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Unknown(#[from] anyhow::Error),
}
