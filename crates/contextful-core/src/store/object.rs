//! The object-store port a bucket sits behind: get, a conditional put, delete and list.
//! Every coordination the bucket carries rests on the conditional put.

/// The condition a put is predicated on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Condition {
    /// Unconditional.
    None,
    /// Only where no object holds the key (`If-None-Match: *`).
    IfNoneMatch,
    /// Only where the object's current ETag equals this one (`If-Match`).
    IfMatch(String),
}

/// What a put did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Put {
    /// Written; the new object's ETag.
    Applied(String),
    /// The condition matched nothing; no byte changed.
    ConditionFailed,
}

/// A failure the backend reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectError {
    /// The backend does not implement the method or header.
    Unsupported(String),
    /// The backend refused the credential.
    Forbidden(String),
    Transport(String),
}

impl std::fmt::Display for ObjectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ObjectError::Unsupported(m) => write!(f, "unsupported: {m}"),
            ObjectError::Forbidden(m) => write!(f, "forbidden: {m}"),
            ObjectError::Transport(m) => write!(f, "transport: {m}"),
        }
    }
}

impl std::error::Error for ObjectError {}

/// An object store: keys to bytes, each object carrying an ETag.
pub trait ObjectStore: Send + Sync {
    /// The object and its ETag, or `None` where no object holds the key.
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError>;
    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError>;
    fn delete(&self, key: &str) -> Result<(), ObjectError>;
    /// Every key beginning with `prefix`, sorted.
    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError>;
}
