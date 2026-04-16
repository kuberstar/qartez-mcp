use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum QartezError {
    #[error("Database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Git error: {0}")]
    Git(#[from] git2::Error),

    #[error("Tree-sitter parse error for {path}: {message}")]
    Parse { path: String, message: String },

    #[error("File not found in index: {0}")]
    FileNotFound(String),

    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    #[error("Project root not detected from {0}")]
    NoProjectRoot(String),

    #[error("Integrity violation: {0}")]
    Integrity(String),
}

pub type Result<T> = std::result::Result<T, QartezError>;
