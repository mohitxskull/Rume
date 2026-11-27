//! Local filesystem object store backend.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::storage::{
    BoxFuture, ObjectKey, ObjectMeta, ObjectStore, ObjectStoreError, PresignedUrl, PutObjectOptions,
};

const META_DIR_NAME: &str = ".rume-meta";
const TEMP_PREFIX: &str = ".tmp-";
const STREAM_HASH_CHUNK_SIZE: usize = 64 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct SidecarMeta {
    #[serde(default)]
    content_type: Option<String>,
    #[serde(default)]
    etag: Option<String>,
}

/// Local filesystem implementation of [`ObjectStore`].
///
/// Enforces [`ObjectKey`] validation, rejects symlink components along any
/// path inside `base_dir`, canonicalizes the nearest existing ancestor
/// directory before writes, and writes objects atomically via temp file + rename.
#[derive(Debug, Clone)]
pub struct LocalFsObjectStore {
    base_dir: PathBuf,
}

impl LocalFsObjectStore {
    /// Creates a new [`LocalFsObjectStore`] rooted at `base_dir`.
    ///
    /// Creates `base_dir` if it does not exist and canonicalizes it.
    pub fn new(base_dir: impl AsRef<Path>) -> Result<Self, ObjectStoreError> {
        std::fs::create_dir_all(base_dir.as_ref())?;
        let canonical = std::fs::canonicalize(base_dir.as_ref())?;
        Ok(Self {
            base_dir: canonical,
        })
    }

    /// Returns the canonicalized root directory of this store.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    fn sidecar_path(&self, key: &str) -> PathBuf {
        let digest = sha256_hex(key.as_bytes());
        self.base_dir
            .join(META_DIR_NAME)
            .join(format!("{digest}.json"))
    }

    async fn read_sidecar(&self, key: &str) -> Result<Option<SidecarMeta>, ObjectStoreError> {
        let path = self.sidecar_path(key);
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let meta = serde_json::from_slice::<SidecarMeta>(&bytes).map_err(|e| {
                    ObjectStoreError::Io(std::io::Error::new(ErrorKind::InvalidData, e))
                })?;
                Ok(Some(meta))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ObjectStoreError::Io(e)),
        }
    }

    async fn write_sidecar(
        &self,
        key: &str,
        content_type: Option<&str>,
        etag: &str,
    ) -> Result<(), ObjectStoreError> {
        let path = self.sidecar_path(key);
        let parent = path.parent().ok_or_else(|| {
            ObjectStoreError::InvalidKey("sidecar path has no parent directory".to_string())
        })?;
        tokio::fs::create_dir_all(parent).await?;
        let payload = serde_json::to_vec(&SidecarMeta {
            content_type: content_type.map(ToString::to_string),
            etag: Some(etag.to_string()),
        })
        .map_err(|e| ObjectStoreError::Io(std::io::Error::new(ErrorKind::InvalidData, e)))?;

        // Why: Write sidecars via temp file + rename in the same `.rume-meta` directory so
        // concurrent `head()`/`list()` readers never observe a half-written JSON sidecar.
        let tmp_path = parent.join(format!("{TEMP_PREFIX}{}", uuid::Uuid::new_v4()));
        if let Err(e) = tokio::fs::write(&tmp_path, &payload).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(ObjectStoreError::Io(e));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(ObjectStoreError::Io(e));
        }
        Ok(())
    }

    async fn remove_sidecar(&self, key: &str) -> Result<(), ObjectStoreError> {
        let path = self.sidecar_path(key);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(ObjectStoreError::Io(e)),
        }
    }

    /// Walks every existing path component inside `base_dir`, rejecting symlinks,
    /// and verifies the canonicalized path remains contained within `base_dir`.
    async fn resolve_existing(&self, key: &ObjectKey) -> Result<PathBuf, ObjectStoreError> {
        validate_local_key(key)?;
        let mut cur = self.base_dir.clone();
        for segment in key.as_str().split('/') {
            cur.push(segment);
            match tokio::fs::symlink_metadata(&cur).await {
                Ok(meta) => {
                    if meta.file_type().is_symlink() {
                        return Err(ObjectStoreError::PermissionDenied(format!(
                            "symlink component rejected in path for key '{}'",
                            key.as_str()
                        )));
                    }
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    return Err(ObjectStoreError::NotFound(key.as_str().to_string()));
                }
                Err(e) => return Err(ObjectStoreError::Io(e)),
            }
        }

        let canonical = tokio::fs::canonicalize(&cur).await?;
        if !canonical.starts_with(&self.base_dir) {
            return Err(ObjectStoreError::PermissionDenied(format!(
                "resolved path escapes base directory for key '{}'",
                key.as_str()
            )));
        }
        Ok(canonical)
    }

    /// Walks path components for a write, rejecting any symlink along existing
    /// components and canonicalizing the nearest existing ancestor directory.
    async fn resolve_for_write(&self, key: &ObjectKey) -> Result<PathBuf, ObjectStoreError> {
        validate_local_key(key)?;
        let mut cur = self.base_dir.clone();
        let mut nearest_existing = self.base_dir.clone();
        let mut saw_missing = false;

        for segment in key.as_str().split('/') {
            cur.push(segment);
            if !saw_missing {
                match tokio::fs::symlink_metadata(&cur).await {
                    Ok(meta) => {
                        if meta.file_type().is_symlink() {
                            return Err(ObjectStoreError::PermissionDenied(format!(
                                "symlink component rejected in path for key '{}'",
                                key.as_str()
                            )));
                        }
                        nearest_existing = cur.clone();
                    }
                    Err(e) if e.kind() == ErrorKind::NotFound => {
                        saw_missing = true;
                    }
                    Err(e) => return Err(ObjectStoreError::Io(e)),
                }
            }
        }

        let canonical_ancestor = tokio::fs::canonicalize(&nearest_existing).await?;
        if !canonical_ancestor.starts_with(&self.base_dir) {
            return Err(ObjectStoreError::PermissionDenied(format!(
                "ancestor directory escapes base directory for key '{}'",
                key.as_str()
            )));
        }

        Ok(cur)
    }
}

impl ObjectStore for LocalFsObjectStore {
    fn put(
        &self,
        key: &ObjectKey,
        data: Vec<u8>,
        options: PutObjectOptions,
    ) -> BoxFuture<'_, Result<ObjectMeta, ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let target = self.resolve_for_write(&key).await?;
            let parent = target.parent().ok_or_else(|| {
                ObjectStoreError::InvalidKey("object key has no parent directory".to_string())
            })?;
            tokio::fs::create_dir_all(parent).await?;

            // Re-verify parent containment after directory creation.
            let canonical_parent = tokio::fs::canonicalize(parent).await?;
            if !canonical_parent.starts_with(&self.base_dir) {
                return Err(ObjectStoreError::PermissionDenied(format!(
                    "parent directory escapes base directory for key '{}'",
                    key.as_str()
                )));
            }

            if let Ok(meta) = tokio::fs::symlink_metadata(&target).await
                && meta.file_type().is_symlink()
            {
                return Err(ObjectStoreError::PermissionDenied(format!(
                    "target path is a symlink for key '{}'",
                    key.as_str()
                )));
            }

            let size = data.len() as u64;
            let etag = sha256_hex(&data);

            let tmp_path = parent.join(format!("{TEMP_PREFIX}{}", uuid::Uuid::new_v4()));
            if let Err(e) = tokio::fs::write(&tmp_path, &data).await {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(ObjectStoreError::Io(e));
            }
            if let Err(e) = tokio::fs::rename(&tmp_path, &target).await {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(ObjectStoreError::Io(e));
            }

            self.write_sidecar(key.as_str(), options.content_type.as_deref(), &etag)
                .await?;

            let last_modified_epoch_secs = tokio::fs::metadata(&target)
                .await
                .ok()
                .and_then(|m| modified_epoch_secs(&m));

            Ok(ObjectMeta {
                key: key.into_inner(),
                size,
                content_type: options.content_type,
                etag: Some(etag),
                last_modified_epoch_secs,
            })
        })
    }

    fn get(&self, key: &ObjectKey) -> BoxFuture<'_, Result<Vec<u8>, ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let path = self.resolve_existing(&key).await?;
            match tokio::fs::read(&path).await {
                Ok(bytes) => Ok(bytes),
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    Err(ObjectStoreError::NotFound(key.into_inner()))
                }
                Err(e) => Err(ObjectStoreError::Io(e)),
            }
        })
    }

    fn head(&self, key: &ObjectKey) -> BoxFuture<'_, Result<ObjectMeta, ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let path = self.resolve_existing(&key).await?;
            let meta = match tokio::fs::metadata(&path).await {
                Ok(m) => m,
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    return Err(ObjectStoreError::NotFound(key.into_inner()));
                }
                Err(e) => return Err(ObjectStoreError::Io(e)),
            };
            let sidecar = self.read_sidecar(key.as_str()).await?;
            let (content_type, stored_etag) = match sidecar {
                Some(s) => (s.content_type, s.etag),
                None => (None, None),
            };
            let etag = match stored_etag {
                Some(etag) => etag,
                None => sha256_file_streaming(&path).await?,
            };

            Ok(ObjectMeta {
                key: key.into_inner(),
                size: meta.len(),
                content_type,
                etag: Some(etag),
                last_modified_epoch_secs: modified_epoch_secs(&meta),
            })
        })
    }

    fn delete(&self, key: &ObjectKey) -> BoxFuture<'_, Result<(), ObjectStoreError>> {
        let key = key.clone();
        Box::pin(async move {
            let path = self.resolve_existing(&key).await?;
            match tokio::fs::remove_file(&path).await {
                Ok(()) => {
                    self.remove_sidecar(key.as_str()).await?;
                    Ok(())
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {
                    Err(ObjectStoreError::NotFound(key.into_inner()))
                }
                Err(e) => Err(ObjectStoreError::Io(e)),
            }
        })
    }

    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<ObjectMeta>, ObjectStoreError>> {
        let prefix = prefix.to_string();
        Box::pin(async move {
            validate_prefix(&prefix)?;

            let mut results = Vec::new();
            let mut stack = vec![self.base_dir.clone()];

            while let Some(dir) = stack.pop() {
                let mut entries = match tokio::fs::read_dir(&dir).await {
                    Ok(e) => e,
                    Err(e) if e.kind() == ErrorKind::NotFound => continue,
                    Err(e) => return Err(ObjectStoreError::Io(e)),
                };

                while let Some(entry) = entries.next_entry().await? {
                    let file_name = entry.file_name();
                    let name_str = file_name.to_string_lossy();
                    if name_str == META_DIR_NAME || name_str.starts_with(TEMP_PREFIX) {
                        continue;
                    }

                    let path = entry.path();
                    let meta = tokio::fs::symlink_metadata(&path).await?;
                    if meta.file_type().is_symlink() {
                        continue;
                    }

                    if meta.is_dir() {
                        stack.push(path);
                    } else if meta.is_file() {
                        let rel = path.strip_prefix(&self.base_dir).map_err(|_| {
                            ObjectStoreError::PermissionDenied(
                                "listed path escapes base directory".to_string(),
                            )
                        })?;
                        let rel_key = rel
                            .components()
                            .map(|c| c.as_os_str().to_string_lossy())
                            .collect::<Vec<_>>()
                            .join("/");

                        if rel_key.starts_with(&prefix) {
                            let sidecar = self.read_sidecar(&rel_key).await?;
                            let (content_type, stored_etag) = match sidecar {
                                Some(s) => (s.content_type, s.etag),
                                None => (None, None),
                            };
                            let etag = match stored_etag {
                                Some(etag) => etag,
                                None => sha256_file_streaming(&path).await?,
                            };
                            results.push(ObjectMeta {
                                key: rel_key,
                                size: meta.len(),
                                content_type,
                                etag: Some(etag),
                                last_modified_epoch_secs: modified_epoch_secs(&meta),
                            });
                        }
                    }
                }
            }

            results.sort_by(|a, b| a.key.cmp(&b.key));
            Ok(results)
        })
    }

    fn presign_get(
        &self,
        _key: &ObjectKey,
        _expires_in_secs: u64,
    ) -> BoxFuture<'_, Result<PresignedUrl, ObjectStoreError>> {
        Box::pin(async {
            Err(ObjectStoreError::Unsupported(
                "presigned URLs are not supported by LocalFsObjectStore".to_string(),
            ))
        })
    }

    fn presign_put(
        &self,
        _key: &ObjectKey,
        _expires_in_secs: u64,
        _options: PutObjectOptions,
    ) -> BoxFuture<'_, Result<PresignedUrl, ObjectStoreError>> {
        Box::pin(async {
            Err(ObjectStoreError::Unsupported(
                "presigned URLs are not supported by LocalFsObjectStore".to_string(),
            ))
        })
    }
}

fn validate_local_key(key: &ObjectKey) -> Result<(), ObjectStoreError> {
    if key
        .as_str()
        .split('/')
        .any(|seg| seg == META_DIR_NAME || seg.starts_with(TEMP_PREFIX))
    {
        return Err(ObjectStoreError::InvalidKey(format!(
            "object key uses reserved internal segment: '{}'",
            key.as_str()
        )));
    }
    Ok(())
}

fn validate_prefix(prefix: &str) -> Result<(), ObjectStoreError> {
    if prefix.starts_with('/')
        || prefix.contains('\\')
        || prefix.contains('\0')
        || prefix.split('/').any(|s| s == "..")
    {
        return Err(ObjectStoreError::InvalidKey(format!(
            "invalid list prefix: '{prefix}'"
        )));
    }
    Ok(())
}

fn encode_digest_hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    encode_digest_hex(&Sha256::digest(data))
}

async fn sha256_file_streaming(path: &Path) -> Result<String, ObjectStoreError> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; STREAM_HASH_CHUNK_SIZE];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(encode_digest_hex(&hasher.finalize()))
}

fn modified_epoch_secs(meta: &std::fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{ObjectKey, ObjectStore, ObjectStoreError, PutObjectOptions};

    #[tokio::test]
    async fn local_fs_store_put_get_head_list_delete_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalFsObjectStore::new(dir.path()).expect("store");
        let key = ObjectKey::new("uploads/2026/hello.txt").expect("valid key");

        let meta = store
            .put(
                &key,
                b"hello rume".to_vec(),
                PutObjectOptions {
                    content_type: Some("text/plain".to_string()),
                },
            )
            .await
            .expect("put");
        assert_eq!(meta.key, "uploads/2026/hello.txt");
        assert_eq!(meta.size, 10);
        assert_eq!(meta.content_type.as_deref(), Some("text/plain"));
        assert!(meta.etag.is_some());

        let got = store.get(&key).await.expect("get");
        assert_eq!(got, b"hello rume");

        let head = store.head(&key).await.expect("head");
        assert_eq!(head.size, 10);
        assert_eq!(head.etag, meta.etag);
        assert_eq!(head.content_type.as_deref(), Some("text/plain"));

        let listed = store.list("uploads/").await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].key, "uploads/2026/hello.txt");

        store.delete(&key).await.expect("delete");
        let err = store.get(&key).await.expect_err("deleted");
        assert!(matches!(err, ObjectStoreError::NotFound(_)));

        assert!(matches!(
            store.presign_get(&key, 60).await,
            Err(ObjectStoreError::Unsupported(_))
        ));
        assert!(matches!(
            store
                .presign_put(&key, 60, PutObjectOptions::default())
                .await,
            Err(ObjectStoreError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn local_fs_store_rejects_path_traversal_and_invalid_keys() {
        assert!(matches!(
            ObjectKey::new(""),
            Err(ObjectStoreError::InvalidKey(_))
        ));
        assert!(matches!(
            ObjectKey::new("/abs/path"),
            Err(ObjectStoreError::InvalidKey(_))
        ));
        assert!(matches!(
            ObjectKey::new("a\\b"),
            Err(ObjectStoreError::InvalidKey(_))
        ));
        assert!(matches!(
            ObjectKey::new("a\0b"),
            Err(ObjectStoreError::InvalidKey(_))
        ));
        assert!(matches!(
            ObjectKey::new("../escape"),
            Err(ObjectStoreError::InvalidKey(_))
        ));
        assert!(matches!(
            ObjectKey::new("a/../escape"),
            Err(ObjectStoreError::InvalidKey(_))
        ));

        #[cfg(unix)]
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let outside = tempfile::tempdir().expect("outside");
            let symlink_dir = dir.path().join("linked");
            std::os::unix::fs::symlink(outside.path(), &symlink_dir).expect("symlink");

            let store = LocalFsObjectStore::new(dir.path()).expect("store");
            let key = ObjectKey::new("linked/secret.txt").expect("syntactically valid key");
            let err = store
                .put(&key, b"nope".to_vec(), PutObjectOptions::default())
                .await
                .expect_err("symlink ancestor must be rejected");
            assert!(matches!(err, ObjectStoreError::PermissionDenied(_)));
        }
    }

    #[tokio::test]
    async fn local_fs_store_rejects_reserved_meta_and_temp_key_segments() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalFsObjectStore::new(dir.path()).expect("store");

        for raw in [
            ".rume-meta/sidecar.json",
            "nested/.rume-meta/sidecar.json",
            ".tmp-1234",
            "uploads/.tmp-upload",
        ] {
            let key = ObjectKey::new(raw).expect("syntactically valid object key");
            assert!(
                matches!(
                    store
                        .put(&key, b"payload".to_vec(), PutObjectOptions::default())
                        .await,
                    Err(ObjectStoreError::InvalidKey(_))
                ),
                "put must reject reserved key '{raw}'"
            );
            assert!(
                matches!(store.get(&key).await, Err(ObjectStoreError::InvalidKey(_))),
                "get must reject reserved key '{raw}'"
            );
            assert!(
                matches!(store.head(&key).await, Err(ObjectStoreError::InvalidKey(_))),
                "head must reject reserved key '{raw}'"
            );
            assert!(
                matches!(
                    store.delete(&key).await,
                    Err(ObjectStoreError::InvalidKey(_))
                ),
                "delete must reject reserved key '{raw}'"
            );
        }
    }

    #[tokio::test]
    async fn local_fs_store_head_and_list_use_sidecar_etag_and_stream_legacy_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LocalFsObjectStore::new(dir.path()).expect("store");
        let key = ObjectKey::new("blobs/sample.bin").expect("valid key");
        let payload = vec![b'x'; 150_000];
        let expected_digest = sha256_hex(&payload);

        let put_meta = store
            .put(
                &key,
                payload,
                PutObjectOptions {
                    content_type: Some("application/octet-stream".to_string()),
                },
            )
            .await
            .expect("put");
        assert_eq!(put_meta.etag.as_deref(), Some(expected_digest.as_str()));

        // Overwrite sidecar with a sentinel ETag to prove head()/list() read ETag from sidecar
        // metadata rather than re-reading the full file payload.
        let sidecar_path = store.sidecar_path(key.as_str());
        let sidecar_raw = tokio::fs::read_to_string(&sidecar_path)
            .await
            .expect("sidecar written");
        assert!(
            sidecar_raw.contains(&expected_digest),
            "sidecar should persist computed etag, got: {sidecar_raw}"
        );
        tokio::fs::write(
            &sidecar_path,
            br#"{"content_type":"application/octet-stream","etag":"sidecar-sentinel-etag"}"#,
        )
        .await
        .expect("overwrite sidecar");

        let head = store.head(&key).await.expect("head");
        assert_eq!(head.etag.as_deref(), Some("sidecar-sentinel-etag"));

        let listed = store.list("blobs/").await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].etag.as_deref(), Some("sidecar-sentinel-etag"));

        // Legacy sidecar without `etag` field falls back to streaming SHA-256.
        tokio::fs::write(
            &sidecar_path,
            br#"{"content_type":"application/octet-stream"}"#,
        )
        .await
        .expect("write legacy sidecar");
        let head_legacy = store.head(&key).await.expect("head legacy");
        assert_eq!(head_legacy.etag.as_deref(), Some(expected_digest.as_str()));
        let list_legacy = store.list("blobs/").await.expect("list legacy");
        assert_eq!(
            list_legacy[0].etag.as_deref(),
            Some(expected_digest.as_str())
        );
    }
}
