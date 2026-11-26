//! Object storage abstraction and backends.

use std::future::Future;
use std::ops::Deref;
use std::pin::Pin;

pub mod local;
#[cfg(feature = "s3")]
pub mod s3;

pub use local::LocalFsObjectStore;
#[cfg(feature = "s3")]
pub use s3::{S3Config, S3ObjectStore};

/// Boxed `Send` future returned by [`ObjectStore`] methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Validated object storage key.
///
/// Rejects empty keys, leading `/`, `\`, null bytes, `.` / `..` segments, and
/// empty path segments (e.g. `a//b` or trailing `/`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectKey(String);

impl ObjectKey {
    /// Validates and constructs an [`ObjectKey`].
    pub fn new(key: impl Into<String>) -> Result<Self, ObjectStoreError> {
        let key = key.into();
        if key.is_empty() {
            return Err(ObjectStoreError::InvalidKey(
                "object key cannot be empty".to_string(),
            ));
        }
        if key.starts_with('/') {
            return Err(ObjectStoreError::InvalidKey(
                "object key cannot start with '/'".to_string(),
            ));
        }
        if key.contains('\\') {
            return Err(ObjectStoreError::InvalidKey(
                "object key cannot contain '\\'".to_string(),
            ));
        }
        if key.contains('\0') {
            return Err(ObjectStoreError::InvalidKey(
                "object key cannot contain null bytes".to_string(),
            ));
        }
        if key
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
        {
            return Err(ObjectStoreError::InvalidKey(format!(
                "object key contains invalid path segment: '{key}'"
            )));
        }
        Ok(Self(key))
    }

    /// Returns the key as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the wrapper and returns the inner string.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Deref for ObjectKey {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<str> for ObjectKey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ObjectKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ObjectKey {
    type Error = ObjectStoreError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ObjectKey {
    type Error = ObjectStoreError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Metadata describing a stored object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub key: String,
    pub size: u64,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub last_modified_epoch_secs: Option<i64>,
}

/// Options applied when writing an object.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PutObjectOptions {
    pub content_type: Option<String>,
}

/// Presigned URL returned by [`ObjectStore::presign_get`] or [`ObjectStore::presign_put`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresignedUrl {
    pub url: String,
    pub method: &'static str,
    pub expires_in_secs: u64,
}

/// Errors returned by [`ObjectStore`] operations.
#[derive(Debug, thiserror::Error)]
pub enum ObjectStoreError {
    #[error("object not found: {0}")]
    NotFound(String),
    #[error("invalid object key: {0}")]
    InvalidKey(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("operation unsupported: {0}")]
    Unsupported(String),
    #[error("storage configuration error: {0}")]
    Configuration(String),
}

/// Unified async object storage backend trait.
pub trait ObjectStore: Send + Sync {
    /// Stores `data` at `key` with the given `options`.
    fn put(
        &self,
        key: &ObjectKey,
        data: Vec<u8>,
        options: PutObjectOptions,
    ) -> BoxFuture<'_, Result<ObjectMeta, ObjectStoreError>>;

    /// Reads the full object payload at `key`.
    fn get(&self, key: &ObjectKey) -> BoxFuture<'_, Result<Vec<u8>, ObjectStoreError>>;

    /// Fetches metadata for the object at `key` without reading its full payload in the return value.
    fn head(&self, key: &ObjectKey) -> BoxFuture<'_, Result<ObjectMeta, ObjectStoreError>>;

    /// Deletes the object at `key`.
    fn delete(&self, key: &ObjectKey) -> BoxFuture<'_, Result<(), ObjectStoreError>>;

    /// Lists objects whose keys start with `prefix`.
    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<ObjectMeta>, ObjectStoreError>>;

    /// Generates a presigned `GET` URL for `key` valid for `expires_in_secs`.
    fn presign_get(
        &self,
        key: &ObjectKey,
        expires_in_secs: u64,
    ) -> BoxFuture<'_, Result<PresignedUrl, ObjectStoreError>>;

    /// Generates a presigned `PUT` URL for `key` valid for `expires_in_secs`.
    fn presign_put(
        &self,
        key: &ObjectKey,
        expires_in_secs: u64,
        options: PutObjectOptions,
    ) -> BoxFuture<'_, Result<PresignedUrl, ObjectStoreError>>;
}
