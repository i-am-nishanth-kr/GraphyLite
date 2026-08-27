use thiserror::Error;

#[derive(Debug, Error)]
pub enum GliteError {
    #[error("storage error: {0}")]
    Storage(String),

    #[error("redb error: {0}")]
    Redb(#[from] redb::Error),

    #[error("redb database error: {0}")]
    Database(#[from] redb::DatabaseError),

    #[error("redb transaction error: {0}")]
    Transaction(#[from] redb::TransactionError),

    #[error("redb table error: {0}")]
    Table(#[from] redb::TableError),

    #[error("redb storage error: {0}")]
    StorageError(#[from] redb::StorageError),

    #[error("redb commit error: {0}")]
    Commit(#[from] redb::CommitError),

    #[error("serialization error: {0}")]
    Serialization(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("execution error: {0}")]
    Execution(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("unsupported: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, GliteError>;
