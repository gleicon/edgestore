//! S3RemoteStore — AWS S3 implementation of `RemoteStore`.
//!
//! Stores content-addressed segments as S3 objects. The `RemoteStore` trait is
//! sync (`Send + Sync`); this implementation bridges to the async AWS SDK via
//! an internal Tokio runtime.
//!
//! ## Cargo feature
//!
//! Requires the `s3` feature on `edgestore-repl`:
//!
//! ```toml
//! [dependencies]
//! edgestore-repl = { version = "1.0", features = ["s3"] }
//! ```
//!
//! ## Path layout
//!
//! ```text
//! s3://{bucket}/{prefix}segments/{hash_hex}.dat
//! ```
//!
//! `prefix` is optional and defaults to `""`. If provided it should end with `/`
//! (e.g. `"mydb/"`).
//!
//! ## LocalStack
//!
//! Set `EDGESTORE_S3_ENDPOINT_URL=http://localhost:4566` for testing against
//! a LocalStack container. Credentials and region are resolved via the
//! standard `aws-config` chain (env vars, `~/.aws/credentials`, IAM roles, etc.).

use std::sync::Arc;

use aws_config::{meta::region::RegionProviderChain, BehaviorVersion};
use aws_sdk_s3::primitives::ByteStream;

use edgestore::error::EdgestoreError;
use edgestore::RemoteStore;

/// S3-backed implementation of `RemoteStore`.
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
                config_loader = config_loader.endpoint_url(url.to_string());
            }

            let sdk_config = config_loader.load().await;
            let mut s3_builder = aws_sdk_s3::config::Builder::from(&sdk_config);
            if endpoint_url.is_some() {
                // LocalStack and MinIO need path-style addressing.
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
        let mut s = String::with_capacity(64);
        for b in hash {
            s.push_str(&format!("{:02x}", b));
        }
        s
    }

    /// Build the S3 object key for a segment hash.
    fn seg_key(&self, hash: &[u8; 32]) -> String {
        format!("{}segments/{}.dat", self.prefix, Self::hash_hex(hash))
    }

    /// Run an async future to completion.
    ///
    /// If the caller is already inside a Tokio runtime this uses
    /// `block_in_place` to avoid nested-runtime panics.
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

        self.block_on(async {
            self.client
                .put_object()
                .bucket(&self.bucket)
                .key(&key)
                .body(ByteStream::from(data.to_vec()))
                .send()
                .await
                .map_err(|e| {
                    EdgestoreError::ReplicationError(format!(
                        "S3 upload failed for {key}: {e}"
                    ))
                })
        })?;

        Ok(())
    }

    fn download(&self, hash: &[u8; 32]) -> Result<Vec<u8>, EdgestoreError> {
        let key = self.seg_key(hash);

        self.block_on(async {
            let output = self
                .client
                .get_object()
                .bucket(&self.bucket)
                .key(&key)
                .send()
                .await
                .map_err(|e| {
                    EdgestoreError::ReplicationError(format!(
                        "S3 download failed for {key}: {e}"
                    ))
                })?;

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
        })
    }

    fn list(&self) -> Result<Vec<[u8; 32]>, EdgestoreError> {
        let prefix = format!("{}segments/", self.prefix);

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
                        let stem = key
                            .strip_prefix(&prefix)
                            .and_then(|s| s.strip_suffix(".dat"));

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

    fn make_store() -> Option<S3RemoteStore> {
        let endpoint = std::env::var("EDGESTORE_S3_ENDPOINT_URL").ok()?;
        let bucket = std::env::var("EDGESTORE_S3_BUCKET")
            .unwrap_or_else(|_| "edgestore-test".to_string());
        S3RemoteStore::new(&bucket, Some("test/"), Some(&endpoint)).ok()
    }

    #[test]
    fn test_upload_download_roundtrip() {
        let Some(store) = make_store() else {
            eprintln!("skip: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash = [0xAAu8; 32];
        let data = b"hello edgestore s3";

        store.upload(&hash, data).expect("upload");
        let got = store.download(&hash).expect("download");
        assert_eq!(got, data);
    }

    #[test]
    fn test_upload_twice_no_error() {
        let Some(store) = make_store() else {
            eprintln!("skip: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash = [0xBBu8; 32];

        // Two uploads with the same hash must not error.
        // In a content-addressed system the hash is derived from the data,
        // so identical hashes imply identical content.
        store.upload(&hash, b"data").expect("first");
        store.upload(&hash, b"data").expect("second");
    }

    #[test]
    fn test_list_returns_uploaded_hashes() {
        let Some(store) = make_store() else {
            eprintln!("skip: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash1 = [0x01u8; 32];
        let hash2 = [0x02u8; 32];
        let hash3 = [0x03u8; 32];

        store.upload(&hash1, b"a").expect("up1");
        store.upload(&hash2, b"b").expect("up2");
        store.upload(&hash3, b"c").expect("up3");

        let listed = store.list().expect("list");

        for h in [&hash1, &hash2, &hash3] {
            assert!(
                listed.contains(h),
                "listed should contain {}",
                S3RemoteStore::hash_hex(h)
            );
        }
    }

    #[test]
    fn test_delete_removes_object() {
        let Some(store) = make_store() else {
            eprintln!("skip: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash = [0xCCu8; 32];

        store.upload(&hash, b"segment data").expect("up");
        store.delete(&hash).expect("del");

        assert!(store.download(&hash).is_err());
    }

    #[test]
    fn test_download_not_found() {
        let Some(store) = make_store() else {
            eprintln!("skip: EDGESTORE_S3_ENDPOINT_URL not set");
            return;
        };

        let hash = [0xDDu8; 32];
        assert!(store.download(&hash).is_err());
    }
}
