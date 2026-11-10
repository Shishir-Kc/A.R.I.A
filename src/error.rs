//! Every error A.R.I.A can raise, in one enum.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AriaError {
    #[error("provider not given: {0}")]
    ProviderNotGiven(String),
    #[error("model name not given: {0}")]
    ModelNameNotGiven(String),
    #[error("API key not given: {0}")]
    ApiKeyNotGiven(String),
    #[error("config file missing: {0}")]
    ConfigFileMissing(String),
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    #[error("directory or url not given")]
    DirectoryNotGiven,
    #[error("encryption key not found")]
    KeysNotFound,
    #[error("additional not found: {0}")]
    AdditionalsNotFound(String),
    #[error("additional '{0}' is not installed")]
    AdditionalsNotInstalled(String),
    #[error("malformed log: {0}")]
    MalformedLog(String),
    #[error("config not found: {0}")]
    ConfigNotFound(String),
    #[error("invalid arguments: {0}")]
    InvalidArgs(String),
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
    #[error("http error: {0}")]
    Http(String),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

impl From<ureq::Error> for AriaError {
    fn from(e: ureq::Error) -> Self {
        AriaError::Http(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, AriaError>;
