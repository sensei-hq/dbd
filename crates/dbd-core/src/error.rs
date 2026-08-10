use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum DbdError {
    #[error("Config error: {0}")]
    Config(String),

    #[error("Parse error in {file}: {message}")]
    Parse { file: PathBuf, message: String },

    #[error("Safety guard: {0}")]
    SafetyGuard(String),

    #[error("GitHub source error: {0}")]
    GitHubSource(String),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, DbdError>;
