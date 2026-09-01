//! Where drained logs go, and the two adapters that get them there (#6533).
//!
//! Why: `run_once` must be testable without a network, and the drain must not
//! grow a second upload path when a later phase adds a backend. One
//! object-safe trait gives both — the tests drive a real `file://` store
//! through the same code the `s3://` destination uses, so a hermetic pass is
//! evidence about the production path rather than about a mock.
//! What: [`LogDestination`], the three-method async trait, and
//! [`ObjectStoreDestination`], the one implementation. It wraps
//! `object_store`'s `LocalFileSystem` for `file://` and `AmazonS3` for `s3://`,
//! selected by [`ObjectStoreDestination::connect`] from a [`DestinationUri`].
//! S3 credentials come from the AWS default provider chain — the same chain
//! `inference::bedrock` uses — bridged into `object_store`'s own
//! `CredentialProvider` by [`AwsChainCredentials`].
//! Test: `super::tests::destination_roundtrip`, `super::tests::destination_roundtrip`,
//! `super::tests::destination_list_is_bounded_and_prefix_scoped`, `super::tests::s3_smoke` (gated).

use std::fmt;
use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use object_store::aws::{AmazonS3Builder, AwsCredential};
use object_store::local::LocalFileSystem;
use object_store::path::Path as StorePath;
use object_store::{
    Attribute, AttributeValue, Attributes, CredentialProvider, ObjectStore, ObjectStoreExt,
    PutOptions, PutPayload,
};

use super::error::DrainError;
use super::uri::DestinationUri;

/// Ceiling on how many entries [`LogDestination::list`] will return.
///
/// Why: `list` exists to reconcile a prefix against the manifest, not to
/// enumerate a bucket. An unbounded listing over a years-old prefix would pull
/// an arbitrary number of entries into memory for a comparison that only ever
/// looks at one session's worth. The cap is the "bounded" in the trait's
/// contract; a caller that hits it is asking the wrong question.
pub const LIST_LIMIT: usize = 10_000;

/// Metadata attached to an object as it is written.
///
/// Why: the drain gzips every body, and an S3 object served without
/// `Content-Encoding: gzip` downloads as bytes no browser or `aws s3 cp` will
/// transparently decompress. Carrying it here keeps the collector (which knows
/// the body is gzipped) and the destination (which writes the header) from
/// having to agree implicitly.
/// What: two optional headers. `#[non_exhaustive]` plus [`Default`] so a later
/// phase can add a field without breaking construction. A destination that has
/// no headers to carry drops them — see
/// [`ObjectStoreDestination`]'s `supports_attributes`.
/// Test: `super::tests::destination_roundtrip` (the body survives the round
/// trip on a local store, which is where attributes are dropped);
/// `super::tests::s3_smoke` is the only path that sends them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PutMeta {
    /// `Content-Type`, e.g. `text/plain`.
    pub content_type: Option<String>,
    /// `Content-Encoding`, `gzip` for every body the collector produces.
    pub content_encoding: Option<String>,
}

impl PutMeta {
    /// The metadata every collected log body is written with: gzipped plain text.
    pub fn gzipped_text() -> Self {
        Self {
            content_type: Some("text/plain".to_string()),
            content_encoding: Some("gzip".to_string()),
        }
    }
}

/// What the destination knows about an object that already exists.
///
/// Deliberately narrower than `object_store::ObjectMeta`: the drain compares
/// sizes and timestamps and never reads an ETag, so exposing one would invite a
/// caller to depend on a field whose semantics differ per backend.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ObjectMeta {
    /// Full object key, including the destination prefix.
    pub key: String,
    /// Size in bytes of the stored (gzipped) body.
    pub size: u64,
    /// Last-modified time as a Unix timestamp in seconds.
    pub last_modified_unix: i64,
}

/// A place drained logs can be written.
///
/// Why: see the module docs — one trait is what lets the hermetic tests
/// exercise the production upload path.
/// What: object-safe (via `#[async_trait]`), so `run_once` takes
/// `&dyn LogDestination` and a later phase can add a backend without touching
/// the drain core. Implementors are `Send + Sync` because the scheduler Phase 3
/// adds will hold one across await points on a multi-threaded runtime.
/// Test: exercised end-to-end through [`ObjectStoreDestination`] in
/// `super::tests::run_once_end_to_end`.
#[async_trait::async_trait]
pub trait LogDestination: Send + Sync + fmt::Debug {
    /// Write `body` at `key`, overwriting whatever was there.
    ///
    /// Overwrite rather than create-if-absent is deliberate: a re-upload only
    /// happens when the manifest says the source changed, and in that case the
    /// newer body is the one that should win.
    async fn put(&self, key: &str, body: Bytes, meta: PutMeta) -> Result<(), DrainError>;

    /// Look up one object, or `Ok(None)` when it does not exist.
    ///
    /// A missing object is `Ok(None)`, never an error — "not uploaded yet" is
    /// the normal case on a first run, not a failure.
    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, DrainError>;

    /// Read one object whole, or `Ok(None)` when it does not exist.
    ///
    /// Why this exists alongside `head`: the manifest's "remote copy wins"
    /// rule (see [`super::manifest::DrainManifest::load`]) needs the remote
    /// document's CONTENT, not merely its existence — `head` can say a
    /// manifest is there but not what it records, which would leave the
    /// authoritative copy unreadable and the rule dead.
    ///
    /// Intended for the manifest only. It buffers the whole object, so it must
    /// not be pointed at a drained log body; `run_once` never does.
    async fn get(&self, key: &str) -> Result<Option<Bytes>, DrainError>;

    /// List objects under `prefix`, capped at [`LIST_LIMIT`] entries.
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, DrainError>;
}

/// The `object_store`-backed destination: `s3://` and `file://`.
///
/// Why: both schemes are the same three operations over a different transport,
/// and `object_store` already abstracts exactly that. Writing two adapters
/// would duplicate the key joining, the error mapping, and the list cap.
/// What: holds an `Arc<dyn ObjectStore>` plus the URI's key prefix. Every
/// method joins the prefix onto the caller's key, so callers pass drain-relative
/// keys and never have to know whether a prefix is in play.
/// Test: `super::tests::destination_roundtrip` and the `run_once` tests drive
/// the `file://` form; `super::tests::s3_smoke` drives the `s3://` form when
/// `TRUSTY_LOG_DRAIN_S3_SMOKE_URI` is set.
pub struct ObjectStoreDestination {
    store: Arc<dyn ObjectStore>,
    prefix: String,
    label: String,
    /// Whether the backing store accepts `PutOptions::attributes`.
    ///
    /// `LocalFileSystem` REJECTS a put carrying attributes outright — it errors
    /// with `NotImplemented` rather than ignoring them — because a file on disk
    /// has nowhere to keep an HTTP header. So the flag is false for `file://`
    /// and true for S3, and [`LogDestination::put`] drops the attributes rather
    /// than failing. Nothing is lost: `Content-Encoding` exists to tell an HTTP
    /// client the body is gzipped, and a local destination has no HTTP client.
    supports_attributes: bool,
}

impl fmt::Debug for ObjectStoreDestination {
    /// Prints the destination label, never the store — an `AmazonS3`'s Debug
    /// carries its credential provider.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectStoreDestination")
            .field("destination", &self.label)
            .finish()
    }
}

impl ObjectStoreDestination {
    /// Build a destination from a parsed URI.
    ///
    /// Why: connecting is fallible and async (the S3 path resolves credentials
    /// through the AWS chain, which may reach IMDS or SSO), so it cannot be a
    /// `From` impl.
    /// What: `file://` creates the directory if absent and wraps a
    /// `LocalFileSystem` rooted there. `s3://` loads the AWS default provider
    /// chain, bridges it into `object_store`'s `CredentialProvider`, and builds
    /// an `AmazonS3` for the bucket. The region comes from the URI's
    /// `?region=` override when present and from the chain otherwise — no
    /// bucket or region is ever hardcoded.
    /// Test: `super::tests::destination_roundtrip` (file),
    /// `super::tests::s3_smoke` (S3, gated on an env var).
    ///
    /// # Errors
    /// - [`DrainError::Io`] when a `file://` root cannot be created or opened.
    /// - [`DrainError::Credentials`] when the AWS chain yields no region.
    /// - [`DrainError::Transport`] when the S3 client cannot be constructed.
    pub async fn connect(uri: &DestinationUri) -> Result<Self, DrainError> {
        match uri {
            DestinationUri::File { path } => {
                std::fs::create_dir_all(path).map_err(|source| DrainError::Io {
                    path: path.clone(),
                    source,
                })?;
                let store = LocalFileSystem::new_with_prefix(path).map_err(|e| DrainError::Io {
                    path: path.clone(),
                    source: std::io::Error::other(e),
                })?;
                Ok(Self {
                    store: Arc::new(store),
                    prefix: String::new(),
                    label: format!("file://{}", path.display()),
                    supports_attributes: false,
                })
            }
            DestinationUri::S3 {
                bucket,
                prefix,
                region,
            } => Self::connect_s3(bucket, prefix, region.as_deref()).await,
        }
    }

    /// Build the S3 store: resolve credentials and region, then hand both to
    /// `AmazonS3Builder`.
    async fn connect_s3(
        bucket: &str,
        prefix: &str,
        region_override: Option<&str>,
    ) -> Result<Self, DrainError> {
        let label = format!("s3://{bucket}/{prefix}");

        // #6533: the same default provider chain `inference::bedrock` loads —
        // env vars, `~/.aws/credentials`, SSO, IMDS — reused rather than
        // reimplemented, per the common-entry-point rule.
        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;

        let region = region_override
            .map(str::to_string)
            .or_else(|| sdk_config.region().map(|r| r.as_ref().to_string()))
            .ok_or_else(|| DrainError::Credentials {
                uri: label.clone(),
                source: "no AWS region: none in the URI's `?region=`, none from the \
                         credential chain (set AWS_REGION or add `?region=` to the URI)"
                    .into(),
            })?;

        let provider =
            sdk_config
                .credentials_provider()
                .ok_or_else(|| DrainError::Credentials {
                    uri: label.clone(),
                    source: "the AWS default provider chain supplied no credentials provider"
                        .into(),
                })?;

        let store = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_region(&region)
            .with_credentials(Arc::new(AwsChainCredentials { inner: provider }))
            .build()
            .map_err(|source| DrainError::Transport {
                op: "connect",
                key: label.clone(),
                source,
            })?;

        Ok(Self {
            store: Arc::new(store),
            prefix: prefix.to_string(),
            label,
            supports_attributes: true,
        })
    }

    /// Join the destination prefix onto a drain-relative key.
    fn absolute(&self, key: &str) -> String {
        if self.prefix.is_empty() {
            key.to_string()
        } else {
            format!("{}/{}", self.prefix, key)
        }
    }

    /// Convert an `object_store::ObjectMeta` into the drain's narrower shape.
    fn convert(meta: object_store::ObjectMeta) -> ObjectMeta {
        ObjectMeta {
            key: meta.location.as_ref().to_string(),
            size: meta.size,
            last_modified_unix: meta.last_modified.timestamp(),
        }
    }
}

#[async_trait::async_trait]
impl LogDestination for ObjectStoreDestination {
    async fn put(&self, key: &str, body: Bytes, meta: PutMeta) -> Result<(), DrainError> {
        let absolute = self.absolute(key);
        let path = StorePath::from(absolute.as_str());

        let mut attributes = Attributes::new();
        // See `supports_attributes`: a local store errors on any attribute at
        // all, so they are dropped rather than sent.
        if self.supports_attributes {
            if let Some(ct) = meta.content_type {
                attributes.insert(Attribute::ContentType, AttributeValue::from(ct));
            }
            if let Some(ce) = meta.content_encoding {
                attributes.insert(Attribute::ContentEncoding, AttributeValue::from(ce));
            }
        }

        let options = PutOptions {
            attributes,
            ..PutOptions::default()
        };

        self.store
            .put_opts(&path, PutPayload::from_bytes(body), options)
            .await
            .map_err(|source| DrainError::Transport {
                op: "put",
                key: absolute,
                source,
            })?;
        Ok(())
    }

    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, DrainError> {
        let absolute = self.absolute(key);
        let path = StorePath::from(absolute.as_str());
        match self.store.head(&path).await {
            Ok(meta) => Ok(Some(Self::convert(meta))),
            // Absent is the normal first-run case, not a failure.
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(source) => Err(DrainError::Transport {
                op: "head",
                key: absolute,
                source,
            }),
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>, DrainError> {
        let absolute = self.absolute(key);
        let path = StorePath::from(absolute.as_str());
        let result = match self.store.get(&path).await {
            Ok(result) => result,
            Err(object_store::Error::NotFound { .. }) => return Ok(None),
            Err(source) => {
                return Err(DrainError::Transport {
                    op: "get",
                    key: absolute,
                    source,
                });
            }
        };
        let bytes = result
            .bytes()
            .await
            .map_err(|source| DrainError::Transport {
                op: "get",
                key: absolute,
                source,
            })?;
        Ok(Some(bytes))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, DrainError> {
        let absolute = self.absolute(prefix);
        let path = StorePath::from(absolute.as_str());
        let mut stream = self.store.list(Some(&path));
        let mut out = Vec::new();
        while let Some(next) = stream.next().await {
            let meta = next.map_err(|source| DrainError::Transport {
                op: "list",
                key: absolute.clone(),
                source,
            })?;
            out.push(Self::convert(meta));
            if out.len() >= LIST_LIMIT {
                tracing::warn!(
                    prefix = %absolute,
                    limit = LIST_LIMIT,
                    "log-drain list hit its entry cap; results truncated"
                );
                break;
            }
        }
        Ok(out)
    }
}

/// Bridges the AWS default credential chain into `object_store`'s provider trait.
///
/// Why: `object_store` signs its own S3 requests and wants an
/// `AwsCredentialProvider`; `aws-config` owns the chain that knows about env
/// vars, profiles, SSO, and IMDS. Reimplementing that chain to satisfy
/// `object_store` would be a second credential resolver in a workspace that
/// already has one, which the common-entry-point rule forbids.
/// What: holds a `SharedCredentialsProvider` and re-fetches on every
/// `get_credential` call, so a rotated or refreshed session token is picked up
/// without rebuilding the store. `object_store` caches the result for the
/// credential's lifetime, so this is not a per-request round trip.
/// Test: `super::tests::s3_smoke` (gated) is the only path that exercises it —
/// there is no way to prove a credential bridge against a local filesystem.
#[derive(Debug)]
struct AwsChainCredentials {
    inner: aws_types::sdk_config::SharedCredentialsProvider,
}

#[async_trait::async_trait]
impl CredentialProvider for AwsChainCredentials {
    type Credential = AwsCredential;

    async fn get_credential(&self) -> object_store::Result<Arc<AwsCredential>> {
        use aws_credential_types::provider::ProvideCredentials;

        let creds = self.inner.provide_credentials().await.map_err(|e| {
            object_store::Error::Unauthenticated {
                path: "aws-credential-chain".to_string(),
                source: Box::new(e),
            }
        })?;

        Ok(Arc::new(AwsCredential {
            key_id: creds.access_key_id().to_string(),
            secret_key: creds.secret_access_key().to_string(),
            token: creds.session_token().map(str::to_string),
        }))
    }
}
