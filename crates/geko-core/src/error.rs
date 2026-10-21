use thiserror::Error;

#[derive(Debug, Error)]
pub enum GekoError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("invalid data: {0}")]
    Data(String),
    #[error("incompatible artifact: {0}")]
    Incompatible(String),
    #[error("configuration error: {0}")]
    Config(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Other(Box<dyn std::error::Error + Send + Sync>),
}

pub type Result<T> = std::result::Result<T, GekoError>;
