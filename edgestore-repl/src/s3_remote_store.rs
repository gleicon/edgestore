//! S3RemoteStore — AWS S3 implementation of `RemoteStore`.
//!
//! Uses the official AWS SDK for Rust (`aws-sdk-s3`) in **blocking mode** via an
//! internal Tokio runtime. This keeps the `RemoteStore` trait sync (`Send + Sync`)
//! while allowing the underlying S3 client to remain fully async.
//!
//! ## LocalStack Testing
//!
//! Set `EDGESTORE_S3_ENDPOINT_URL=http://localhost:4566` to point at a LocalStack
//! container. The AWS region and credentials are resolved via the standard
//! `aws-config` chain (environment variables, `~/.aws/credentials`, IAM roles, etc.).
//!
//! ## Path Layout
//!
//! ```text
//! s3://{bucket}/{prefix}segments/{hash_hex}.dat
//! ```
//!
//! `prefix` is optional and defaults to `""`. If provided, it should end with `/`
//! (e.g. `"mydb/"`).
//!
//! ## Async-to-Sync Bridge
//!
//! `S3RemoteStore` creates a dedicated multi-thread Tokio runtime on
//! construction. If called from within an existing Tokio runtime, the call is
//! offloaded to `tokio::task::spawn_blocking` to avoid the
//! "cannot start a runtime from within a runtime" panic.
//!
//! Requires the `s3` Cargo feature on `edgestore-repl`:
//!
//! ```toml
//! [dependencies]
//! edgestore-repl = { version = "1.0", features = ["s3"] }
//! ```

use std::sync::Arc;

use aws_config::{meta::region::RegionProviderChain, BehaviorVersion};
use aws_sdk_s3::{
    primitives::ByteStream,
    types::ChecksumAlgorithm,
};

use edgestore::error::EdgestoreError;
use edgestore::RemoteStore;

/// S3-backed implementation of `RemoteStore`.
///
/// Stores content-addressed segments as S3 objects. `upload` is idempotent:
/// a `HeadObject` check skips the write if the object already exists.
///
/// # Example
///
/// ```no_run
/// use edgestore::RemoteStore;
/// use edgestore_repl::S3RemoteStore;
///
/// let store = S3RemoteStore::new(
///     "my-bucket",
///     Some("mydb/"),
///     Some("http://localhost:4566"),
/// ).expect("S3RemoteStore::new");
///
/// let hash = [0x42u8; 32];
/// let data = b"hello edgestore";
/// store.upload(&hash, data).expect("upload");
/// ```
pub struct S3RemoteStore {
    client: aws_sdk_s3::Client,
    bucket: String,
    prefix: String,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl S3RemoteStore {
    /// Create a new `S3RemoteStore`.
    ///
    /// # Arguments
    ///
    /// * `bucket` — S3 bucket name.
    /// * `prefix` — Optional key prefix. Should end with `/` if non-empty
    ///   (e.g. `"mydb/"`). Pass `None` for a flat layout.
    /// * `endpoint_url` — Optional custom endpoint URL. Use
    ///   `Some("http://localhost:4566")` for LocalStack. Pass `None` to use
    ///   the standard AWS endpoint for the resolved region.
    ///
    /// # Errors
    ///
    /// Returns `EdgestoreError::ReplicationError` if the Tokio runtime cannot
    /// be created or the AWS client fails to initialize.
    pub fn new(
        bucket: impl Into<String>,
        prefix: Option<&str>,
        endpoint_url: Option<&str>,
    ) -> Result<Self, EdgestoreError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| {
                EdgestoreError::ReplicationError(format!(
                    "failed to create Tokio runtime: {e}"
                ))
            })?;

        let client = runtime.block_on(async {
            let region_provider =
                RegionProviderChain::default_provider().or_else("us-east-1");

            let mut config_loader =
                aws_config::defaults(BehaviorVersion::latest())
                    .region(region_provider);

            if let Some(url) = endpoint_url {
                config_loader =
                    config_loader.endpoint_url(url.to_string());
            }

            let sdk_config = config_loader.load().await;
            let mut s3_builder = aws_sdk_s3::config::Builder::from(&sdk_config);
            if endpoint_url.is_some() {
                s3_builder = s3_builder.force_path_style(true);
            }
            aws_sdk_s3::Client::from_conf(s3_builder.build())
        });

        Ok(Self {
            client,
            bucket: bucket.into(),
            prefix: prefix.unwrap_or("").to_string(),
            runtime: Arc::new(runtime),
        })
    }

    /// Encode a 32-byte hash as a 64-character lowercase hex string.
    fn hash_hex(hash: &[u8; 32]) -> String {
        hash.iter()
            .map(|b| format!("{:02x}", b))
            .collect::<String>()
    }

    /// Build the S3 object key for a segment hash.
    fn seg_key(&self, hash: &[u8; 32]) -> String {
        format!("{}segments/{}.dat", self.prefix, Self::hash_hex(hash))
    }

    /// Run an async future to completion, handling the case where the caller
    /// is already inside a Tokio runtime.
    ///
    /// Uses `tokio::task::block_in_place` when nested inside an existing runtime
    /// to avoid the "cannot start a runtime from within a runtime" panic.
    fn block_on<F, R>(&self, future: F) -> R
    where
        F: std::future::Future<Output = R> + Send,
        R: Send,
    {
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                tokio::task::block_in_place(|| handle.block_on(future))
            }
            Err(_) => self.runtime.block_on(future),
        }
    }
}

impl RemoteStore for S3RemoteStore {
    fn upload(&self, hash: &[u8; 32], data: &[u8]) -> Result<(), EdgestoreError> {
        let key = self.seg_key(hash);

        // Idempotency check: skip if object already exists.
        let head_result = self.block_on(async {
            self.client
                .head_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
        });

        if head_result.is_ok() {
            return Ok(());
        }

        // Object does not exist (or we got an error other than NotFound —
        // proceed with upload anyway and let PutObject fail if truly broken).
        self.block_on(async {
            let body = ByteStream::from(data.to_vec());
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(body)
                .checksum_algorithm(ChecksumAlgorithm::Crc32)
                .send()
                .await
                .map_err(|e| {
                    let err = format!("S3 upload failed for {key}: {e}");
                    EdgestoreError::ReplicationError(err)
                })
        })?;

        Ok(())
    }

    fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
        let key = self.seg_key(hash);

        let output = self.block_on(async {
            self.client
                .get_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| {
                    let err = format!("S3 download failed for {key}: {e}");
                    EdgestoreError::ReplicationError(err)
                })
        })?;

        let data = self.block_on(async {
            output
                .body
                .collect()
                .await
                .map(|d| d.into_bytes().to_vec())
                .map_err(|e| {
                    EdgestoreError::ReplicationError(format!(
                        "S3 body stream error for {key}: {e}"
                    ))
                })
        })?;

        Ok(data)
    }

    fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError> {
        let prefix = format!("{}segments/", self.prefix);
        let suffix = ".dat";

        let mut hashes = Vec::new();
        let mut continuation_token: Option<String> = None;

        loop {
            let mut req = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(&prefix);

            if let Some(token) = continuation_token {
                req = req.continuation_token(token);
            }

            let output = self.block_on(async {
                req.send().await.map_err(|e| {
                    EdgestoreError::ReplicationError(format!(
                        "S3 list failed: {e}"
                    ))
                })
            })?;

            if let Some(contents) = output.contents {
                for obj in contents {
                    if let Some(key) = obj.key {
                        // Strip prefix and suffix, leaving just the hash hex.
                        let stem = key
                            .strip_prefix(&prefix)
                            .and_then(|s| s.strip_suffix(suffix));

                        if let Some(stem) = stem {
                            if stem.len() == 64 {
                                let parsed: Option<[u8; 32]> = (0..32)
                                    .map(|i| {
                                        u8::from_str_radix(
                                            &stem[i * 2..i * 2 + 2],
                                            16,
                                        )
                                        .ok()
                                    })
                                    .collect::<Option<Vec<u8>>>()
                                    .and_then(|v| v.try_into().ok());

                                if let Some(hash) = parsed {
                                    hashes.push(hash);
                                }
                            }
                        }
                    }
                }
            }

            if output.is_truncated.unwrap_or(false) {
                continuation_token = output.next_continuation_token;
            } else {
                break;
            }
        }

        Ok(hashes)
    }

    fn delete(&self, hash: &[u8; 32]) -> Result<(), EdgestoreError> {
        let key = self.seg_key(hash);

        self.block_on(async {
            self.client
                .delete_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| {
                    EdgestoreError::ReplicationError(format!(
                        "S3 delete failed for {key}: {e}"
                    ))
                })
        })?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a store pointing at LocalStack using environment variables.
    fn make_localstack_store() -> Option<S3RemoteStore> {
        let endpoint = std::env::var("EDGESTORE_S3_ENDPOINT_URL").ok()?;
        let bucket = std::env::var("EDGESTORE_S3_BUCKET").unwrap_or_else(|_| "edgestore-test".to_string());

        S3RemoteStore::new(&bucket, Some("test/"), Some(&endpoint)).ok()
    }

    #[test]
    fn test_upload_download_roundtrip() {
        let Some(store) = make_localstack_store() else {
            eprintln!("Skipping S3 test: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash = [0xAAu8; 32];
        let data = b"hello edgestore s3";

        store.upload(&hash, data).expect("upload");
        let got = store.download(&hash).expect("download");
        assert_eq!(got, data);
    }

    #[test]
    fn test_upload_idempotent() {
        let Some(store) = make_localstack_store() else {
            eprintln!("Skipping S3 test: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash = [0xBBu8; 32];
        let data = b"original";

        store.upload(&hash, data).expect("first upload");
        store.upload(&hash, b"different").expect("second upload (idempotent)");

        let got = store.download(&hash).expect("download after idempotent upload");
        assert_eq!(got, data);
    }

    #[test]
    fn test_list_returns_uploaded_hashes() {
        let Some(store) = make_localstack_store() else {
            eprintln!("Skipping S3 test: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash1 = [0x01u8; 32];
        let hash2 = [0x02u8; 32];
        let hash3 = [0x03u8; 32];

        store.upload(&hash1, b"a").expect("upload 1");
        store.upload(&hash2, b"b").expect("upload 2");
        store.upload(&hash3, b"c").expect("upload 3");

        let listed = store.list().expect("list");

        // The bucket may contain objects from other tests; verify that
        // the three hashes we just uploaded are present.
        for h in [&hash1, &hash2, &hash3] {
            assert!(
                listed.contains(h),
                "listed hashes should contain {}",
                S3RemoteStore::hash_hex(h)
            );
        }
    }

    #[test]
    fn test_delete_removes_object() {
        let Some(store) = make_localstack_store() else {
            eprintln!("Skipping S3 test: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash = [0xCCu8; 32];

        store.upload(&hash, b"segment data").expect("upload");
        store.delete(&hash).expect("delete");

        let result = store.download(&hash);
        assert!(result.is_err(), "download after delete should return Err");
    }

    #[test]
    fn test_download_not_found() {
        let Some(store) = make_localstack_store() else {
            eprintln!("Skipping S3 test: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash = [0xDDu8; 32];

        let result = store.download(&hash);
        assert!(result.is_err(), "download of non-existent hash should return Err");
    }
}
