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
//! `CredentialProvider` by [`AwsChainCredentials`]. A URI carrying `?profile=`
//! or `?role_arn=` narrows that to one named profile or an assumed role
//! (#6657); see [`resolve_s3_auth`].
//! Test: `super::tests::destination_roundtrip`, `super::tests::destination_roundtrip`,
//! `super::tests::destination_list_is_bounded_and_prefix_scoped`, `super::tests::s3_smoke` (gated).

use std::fmt;
use std::sync::Arc;

use aws_config::profile::ProfileFileCredentialsProvider;
use aws_config::profile::region::ProfileFileRegionProvider;
use aws_config::sts::AssumeRoleProvider;
use aws_types::region::Region;
use aws_types::sdk_config::SharedCredentialsProvider;
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

/// The profile-file set `ProfileFileCredentialsProvider` and
/// `ProfileFileRegionProvider` are pointed at.
///
/// #6657: `aws-config` deprecated this alias in favour of
/// `aws_runtime::env_config::file::EnvConfigFiles`, but its own
/// `profile_files()` builder methods still take the old name, so it cannot be
/// avoided. Declaring `aws-runtime` directly to dodge one deprecation would pin
/// an SDK-internal crate's version in the workspace manifest.
#[allow(deprecated)]
pub(super) type ProfileFiles = aws_config::profile::profile_file::ProfileFiles;

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

    /// Stable, filesystem-safe identity of this destination for local state.
    ///
    /// Why: a skip decision recorded against one destination says nothing about
    /// another. The local manifest cache is stored under this string, so
    /// switching buckets can no longer reuse the record written for the
    /// previous one (#6548). It belongs on the DESTINATION rather than in a
    /// caller-supplied config field because an id the caller passes in can
    /// drift from the destination it names, and that drift is the bug again.
    ///
    /// What: any value that differs whenever two destinations could hold
    /// different objects, and that is safe as a single path segment.
    /// [`ObjectStoreDestination`] returns [`DestinationUri::cache_namespace`].
    ///
    /// Test: `super::tests::run_once_reuploads_when_the_destination_changes`.
    fn cache_namespace(&self) -> &str;
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
    /// `DestinationUri::cache_namespace` for the URI this was built from (#6548).
    cache_namespace: String,
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
    /// `LocalFileSystem` rooted there. `s3://` resolves an identity through
    /// [`resolve_s3_auth`], bridges it into `object_store`'s
    /// `CredentialProvider`, and builds an `AmazonS3` for the bucket. The
    /// region comes from the URI's `?region=` override when present and from
    /// the resolved identity otherwise — no bucket or region is ever hardcoded.
    /// Test: `super::tests::destination_roundtrip` (file),
    /// `super::tests::s3_smoke` (S3, gated on an env var).
    ///
    /// # Errors
    /// - [`DrainError::Io`] when a `file://` root cannot be created or opened.
    /// - [`DrainError::Credentials`] when no region or no credentials resolve.
    /// - [`DrainError::Transport`] when the S3 client cannot be constructed.
    pub async fn connect(uri: &DestinationUri) -> Result<Self, DrainError> {
        // #6548: derived here, from the URI, so no caller can supply an id that
        // disagrees with the destination it is naming.
        let cache_namespace = uri.cache_namespace();
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
                    cache_namespace,
                })
            }
            DestinationUri::S3 {
                bucket,
                prefix,
                region,
                profile,
                role_arn,
            } => {
                Self::connect_s3(
                    bucket,
                    prefix,
                    S3AuthRequest {
                        region: region.as_deref(),
                        profile: profile.as_deref(),
                        role_arn: role_arn.as_deref(),
                        profile_files: None,
                    },
                    cache_namespace,
                )
                .await
            }
        }
    }

    /// Build the S3 store: resolve an identity and region, then hand both to
    /// `AmazonS3Builder`.
    async fn connect_s3(
        bucket: &str,
        prefix: &str,
        auth: S3AuthRequest<'_>,
        cache_namespace: String,
    ) -> Result<Self, DrainError> {
        let label = format!("s3://{bucket}/{prefix}");
        let resolved = resolve_s3_auth(&label, auth).await?;

        let store = AmazonS3Builder::new()
            .with_bucket_name(bucket)
            .with_region(&resolved.region)
            .with_credentials(Arc::new(AwsChainCredentials {
                inner: resolved.provider,
            }))
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
            cache_namespace,
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

    fn cache_namespace(&self) -> &str {
        &self.cache_namespace
    }
}

/// What a caller knows about an `s3://` destination's identity (#6657).
///
/// Why a struct rather than four arguments: `profile_files` exists only so a
/// test can supply profile text in memory, and a bare `Option<&ProfileFiles>`
/// in the fourth position of a call is unreadable at every real call site.
/// What: `profile_files` is `None` everywhere in production, which means the
/// SDK's own file discovery (`~/.aws/config`, `~/.aws/credentials`, and the
/// `AWS_CONFIG_FILE` / `AWS_SHARED_CREDENTIALS_FILE` overrides).
/// Test: `super::tests::two_profiles_resolve_to_different_identities`.
pub(super) struct S3AuthRequest<'a> {
    /// `?region=` from the URI, when the operator pinned one.
    pub region: Option<&'a str>,
    /// `?profile=` from the URI.
    pub profile: Option<&'a str>,
    /// `?role_arn=` from the URI.
    pub role_arn: Option<&'a str>,
    /// Profile-file override. `None` in production; a test injects text here.
    pub profile_files: Option<&'a ProfileFiles>,
}

/// A resolved S3 identity: which region to sign for, and with what credentials.
pub(super) struct S3Auth {
    /// Region the bucket is addressed in.
    pub region: String,
    /// Credentials every request is signed with.
    pub provider: SharedCredentialsProvider,
}

impl fmt::Debug for S3Auth {
    /// Prints the region only. A credential provider's own `Debug` can carry
    /// key material, and this type ends up in test failure messages.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3Auth")
            .field("region", &self.region)
            .finish()
    }
}

/// Resolve the region and credentials one `s3://` destination signs with.
///
/// Why: one daemon now drains to several destinations, and the owner ruling
/// that this project's logs belong in one specific AWS account is only
/// enforceable if a destination can name its own identity. Leaving every
/// destination on the process-wide default chain meant whichever credentials
/// the daemon happened to start with wrote to all of them (#6657).
///
/// What: three cases, in order.
/// - `?profile=<name>` — credentials come from THAT profile alone, through
///   `ProfileFileCredentialsProvider`, and the region falls back to the same
///   profile's `region`. The default chain is not consulted, so an
///   `AWS_ACCESS_KEY_ID` in the daemon's environment cannot override a pinned
///   profile. That is the whole point of pinning one.
/// - `?role_arn=<arn>` — the base identity above (or the default chain when no
///   profile is named) signs an STS `AssumeRole`, and the assumed role's
///   credentials are what reach S3. No STS call happens here: the provider is
///   lazy, so a bad ARN surfaces on the first upload rather than at connect.
/// - neither — the AWS default provider chain, exactly as before.
///
/// Region resolution is `?region=` → the identity's own region → refusal. It is
/// never defaulted to a literal, because a wrong region is a request signed for
/// a bucket that is not there.
///
/// Test: `super::tests::two_profiles_resolve_to_different_identities`,
/// `super::tests::a_profile_without_a_region_is_refused`,
/// `super::tests::a_role_arn_resolves_to_an_assumed_role_identity`,
/// `super::tests::a_profile_and_a_role_arn_do_not_collapse_onto_the_profile`.
///
/// # Errors
/// [`DrainError::Credentials`] when no region resolves, or when the default
/// chain yields no credentials provider.
pub(super) async fn resolve_s3_auth(
    label: &str,
    auth: S3AuthRequest<'_>,
) -> Result<S3Auth, DrainError> {
    let (identity_region, base) = match auth.profile {
        Some(name) => resolve_profile_identity(name, auth.profile_files).await,
        None => resolve_default_chain(label).await?,
    };

    let region = auth
        .region
        .map(str::to_string)
        .or(identity_region)
        .ok_or_else(|| DrainError::Credentials {
            uri: label.to_string(),
            source: "no AWS region: none in the URI's `?region=`, none from the \
                     credential chain or the named profile (set AWS_REGION, give the \
                     profile a `region`, or add `?region=` to the URI)"
                .into(),
        })?;

    let provider = match auth.role_arn {
        None => base,
        Some(arn) => assume_role(arn, &region, base).await,
    };

    Ok(S3Auth { region, provider })
}

/// Credentials and region from one named `~/.aws` profile.
///
/// Both halves read the same profile files, so a profile that sets `region` in
/// `~/.aws/config` needs no `?region=` in the URI.
async fn resolve_profile_identity(
    name: &str,
    files: Option<&ProfileFiles>,
) -> (Option<String>, SharedCredentialsProvider) {
    let mut credentials = ProfileFileCredentialsProvider::builder().profile_name(name);
    let mut region = ProfileFileRegionProvider::builder().profile_name(name);
    if let Some(files) = files {
        credentials = credentials.profile_files(files.clone());
        region = region.profile_files(files.clone());
    }
    // `ProvideRegion` is the trait `region()` lives on.
    use aws_config::meta::region::ProvideRegion;
    let region = region
        .build()
        .region()
        .await
        .map(|r| r.as_ref().to_string());
    (region, SharedCredentialsProvider::new(credentials.build()))
}

/// Credentials and region from the AWS default provider chain.
///
/// #6533: the same chain `inference::bedrock` loads — env vars,
/// `~/.aws/credentials`, SSO, IMDS — reused rather than reimplemented, per the
/// common-entry-point rule.
async fn resolve_default_chain(
    label: &str,
) -> Result<(Option<String>, SharedCredentialsProvider), DrainError> {
    let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .load()
        .await;
    let region = sdk_config.region().map(|r| r.as_ref().to_string());
    let provider = sdk_config
        .credentials_provider()
        .ok_or_else(|| DrainError::Credentials {
            uri: label.to_string(),
            source: "the AWS default provider chain supplied no credentials provider".into(),
        })?;
    Ok((region, provider))
}

/// Wrap `base` in an STS `AssumeRole` for `arn`.
///
/// The region and credentials are pinned onto the `SdkConfig` the STS client is
/// built from, so building it resolves nothing on its own — no IMDS probe, no
/// network call until the first credential fetch.
async fn assume_role(
    arn: &str,
    region: &str,
    base: SharedCredentialsProvider,
) -> SharedCredentialsProvider {
    let sts_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(Region::new(region.to_string()))
        .credentials_provider(base)
        .load()
        .await;
    SharedCredentialsProvider::new(
        AssumeRoleProvider::builder(arn)
            .configure(&sts_config)
            .build()
            .await,
    )
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
    inner: SharedCredentialsProvider,
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
