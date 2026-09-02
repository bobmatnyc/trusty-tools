//! Destination-URI parsing for the log drain (#6533).
//!
//! Why: the drain accepts exactly two destinations and reserves two more. A
//! general URI crate would accept every scheme and push the "is this one of
//! ours" question into the caller, where each call site would answer it
//! slightly differently. A closed scheme enum answers it once, and makes
//! `gs://` and `az://` produce a message naming what IS supported rather than
//! a parse failure.
//! What: [`DestinationUri`], a parsed destination, and [`DestinationScheme`],
//! the closed set of schemes the parser recognises. Parsing is hand-rolled
//! over `str` — split on `://`, dispatch on the scheme, then parse the
//! remainder per-scheme. No generic URI crate is involved.
//! Test: `super::tests::uri_table_accepts`, `super::tests::uri_table_rejects`,
//! `super::tests::uri_reserved_schemes`, `super::tests::uri_region_override`,
//! `super::tests::uri_accepts_profile_and_role_arn`.

use std::path::PathBuf;

use super::error::DrainError;
use super::pipeline::hex_digest;

/// The scheme half of a destination URI.
///
/// Why: naming the reserved schemes in the same enum as the supported ones is
/// what lets the parser answer `gs://` with [`DrainError::UnsupportedScheme`]
/// instead of a generic syntax error.
/// What: two supported variants and two reserved ones. `Gs` and `Az` are
/// recognised by [`DestinationScheme::parse`] and then rejected — no backend
/// exists behind them, and `object_store`'s `gcp`/`azure` features are
/// deliberately off in the workspace manifest.
/// Test: `super::tests::uri_reserved_schemes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DestinationScheme {
    /// `s3://bucket/prefix` — Amazon S3, credentials from the AWS chain.
    S3,
    /// `file:///abs/path` — a local directory, used by the hermetic tests.
    File,
    /// `gs://…` — recognised and rejected. Reserved for a later phase.
    Gs,
    /// `az://…` — recognised and rejected. Reserved for a later phase.
    Az,
}

impl DestinationScheme {
    /// Map a scheme string to its variant, or `None` if it is not one of the four.
    fn parse(scheme: &str) -> Option<Self> {
        match scheme {
            "s3" => Some(Self::S3),
            "file" => Some(Self::File),
            "gs" => Some(Self::Gs),
            "az" => Some(Self::Az),
            _ => None,
        }
    }
}

/// A parsed, validated log-drain destination.
///
/// Why: the two supported schemes carry different data — S3 needs a bucket and
/// an optional region, a local directory needs a path — so a single struct with
/// nullable fields would let an impossible combination compile. The enum makes
/// the adapter's `match` total.
/// What: [`DestinationUri::parse`] is the only constructor. The `prefix` on the
/// S3 variant is the key prefix every drained object is written beneath; it is
/// normalised to carry no leading or trailing `/`, and is empty for a
/// bucket-root destination.
/// Test: `super::tests::uri_table_accepts` covers every accepted form,
/// `super::tests::uri_table_rejects` every rejected one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DestinationUri {
    /// An S3 bucket, optionally rooted at a key prefix.
    S3 {
        /// Bucket name. Never empty — the parser rejects `s3:///prefix`.
        bucket: String,
        /// Key prefix, `/`-normalised and possibly empty.
        prefix: String,
        /// Region override from `?region=…`. `None` defers to the AWS chain.
        region: Option<String>,
        /// Named `~/.aws` profile from `?profile=…` (#6657).
        ///
        /// `Some` means the profile is the WHOLE identity: the adapter reads
        /// credentials from that profile alone and never consults the default
        /// chain, so `AWS_ACCESS_KEY_ID` in the daemon's environment cannot
        /// silently re-point one destination at another account.
        profile: Option<String>,
        /// Role to assume from `?role_arn=…` (#6657).
        ///
        /// Combines with `profile`: the profile (or the default chain when
        /// there is none) supplies the base credentials the STS
        /// `AssumeRole` call is signed with.
        role_arn: Option<String>,
    },
    /// A local directory. Used by the hermetic tests and by any operator who
    /// wants the drain's output on disk before pointing it at a bucket.
    File {
        /// Absolute path to the directory that becomes the destination root.
        path: PathBuf,
    },
}

impl DestinationUri {
    /// Parse a destination URI.
    ///
    /// Why: every entry point that accepts a destination string routes through
    /// this one function, so the accepted grammar cannot drift between the
    /// config reader, the CLI, and the tests.
    /// What: splits at the first `://`, resolves the scheme through
    /// [`DestinationScheme::parse`], then parses the remainder per scheme.
    /// An `s3://` URI accepts exactly three query parameters — `region`,
    /// `profile`, and `role_arn` (#6657) — in any combination and any order.
    /// Every other key is rejected, and a repeated key is rejected rather than
    /// letting the last one win.
    /// Test: `super::tests::uri_table_accepts`, `super::tests::uri_table_rejects`,
    /// `super::tests::uri_reserved_schemes`, `super::tests::uri_region_override`,
    /// `super::tests::uri_accepts_profile_and_role_arn`.
    ///
    /// # Errors
    /// - [`DrainError::UnsupportedScheme`] for `gs://` and `az://`, and for any
    ///   scheme outside the four this parser knows.
    /// - [`DrainError::Uri`] for a missing `://`, an empty bucket, a
    ///   non-absolute `file://` path, or an unrecognised query parameter.
    pub fn parse(uri: &str) -> Result<Self, DrainError> {
        let trimmed = uri.trim();
        let Some((scheme, rest)) = trimmed.split_once("://") else {
            return Err(DrainError::Uri {
                uri: uri.to_string(),
                reason: "expected `<scheme>://…`, found no `://` separator".to_string(),
            });
        };

        // Schemes are case-insensitive per RFC 3986; the rest of the URI is not.
        let scheme_lower = scheme.to_ascii_lowercase();
        let Some(known) = DestinationScheme::parse(&scheme_lower) else {
            return Err(DrainError::UnsupportedScheme {
                scheme: scheme_lower,
                uri: uri.to_string(),
            });
        };

        match known {
            DestinationScheme::S3 => Self::parse_s3(uri, rest),
            DestinationScheme::File => Self::parse_file(uri, rest),
            // #6533: reserved on purpose — recognised so the message names what
            // IS supported, but no backend is compiled behind either.
            DestinationScheme::Gs | DestinationScheme::Az => Err(DrainError::UnsupportedScheme {
                scheme: scheme_lower,
                uri: uri.to_string(),
            }),
        }
    }

    /// Parse the remainder of an `s3://` URI:
    /// `bucket[/prefix][?region=…&profile=…&role_arn=…]`.
    fn parse_s3(uri: &str, rest: &str) -> Result<Self, DrainError> {
        let (path_part, query) = match rest.split_once('?') {
            Some((p, q)) => (p, Some(q)),
            None => (rest, None),
        };

        let (bucket, prefix) = match path_part.split_once('/') {
            Some((b, p)) => (b, p),
            None => (path_part, ""),
        };

        if bucket.is_empty() {
            return Err(DrainError::Uri {
                uri: uri.to_string(),
                reason: "bucket name is empty — expected `s3://<bucket>[/<prefix>]`".to_string(),
            });
        }

        let params = match query {
            Some(q) => parse_s3_query(uri, q)?,
            None => S3Params::default(),
        };

        Ok(Self::S3 {
            bucket: bucket.to_string(),
            prefix: normalise_prefix(prefix),
            region: params.region,
            profile: params.profile,
            role_arn: params.role_arn,
        })
    }

    /// Parse the remainder of a `file://` URI.
    ///
    /// Accepts the RFC-8089 local form `file:///abs/path` (empty authority), so
    /// after the `://` split the remainder begins with `/`.
    fn parse_file(uri: &str, rest: &str) -> Result<Self, DrainError> {
        if rest.contains('?') {
            return Err(DrainError::Uri {
                uri: uri.to_string(),
                reason: "`file://` takes no query parameters".to_string(),
            });
        }
        if !rest.starts_with('/') {
            return Err(DrainError::Uri {
                uri: uri.to_string(),
                reason: "expected an absolute path — `file:///abs/path`, with three slashes"
                    .to_string(),
            });
        }
        // A trailing `/` is cosmetic; `PathBuf` treats `/a/b/` and `/a/b` alike,
        // but trimming keeps the Debug output stable across equivalent inputs.
        let path = rest.trim_end_matches('/');
        if path.is_empty() {
            return Err(DrainError::Uri {
                uri: uri.to_string(),
                reason: "path is the filesystem root — refusing to drain into `/`".to_string(),
            });
        }
        Ok(Self::File {
            path: PathBuf::from(path),
        })
    }

    /// The key prefix every object written to this destination sits beneath.
    ///
    /// Empty for `file://`, where the URI's path is the store root and keys are
    /// already relative to it.
    pub fn prefix(&self) -> &str {
        match self {
            Self::S3 { prefix, .. } => prefix,
            Self::File { .. } => "",
        }
    }

    /// Stable, filesystem-safe identity of this destination for local state.
    ///
    /// Why: the local manifest cache used to live at a path built from the
    /// identity alone, so pointing one session at a second bucket reused the
    /// record written for the first and skipped every file that record listed —
    /// 86 files that never reached the new bucket (#6548). A skip decision is
    /// only valid for the destination it was made against, so the destination
    /// has to be part of where that decision is stored.
    ///
    /// What: `<scheme>-<first 16 hex chars of SHA-256(canonical form)>`. The
    /// canonical form carries scheme, bucket-or-path, key prefix, and the
    /// identity (`profile`, `role_arn`) — everything that changes WHICH objects
    /// a destination holds or can see. `?region=` is excluded on purpose: a
    /// region override changes which endpoint serves a bucket, never its
    /// contents, so adding one must not orphan a valid cache. The identity is
    /// NOT excluded, because two identities against one bucket can see
    /// different objects, and the cheap way to be wrong is the safe one — an
    /// unnecessary split re-uploads, a wrong share skips (#6657).
    /// The value is hashed rather than spelled out because a key prefix and a
    /// filesystem path are both arbitrary strings, and a cache directory needs
    /// one segment with no `/`, no `..`, and no length surprise.
    ///
    /// Test: `super::tests::cache_namespace_separates_destinations`,
    /// `super::tests::cache_namespace_ignores_the_region_override`,
    /// `super::tests::cache_namespace_separates_identities`.
    pub fn cache_namespace(&self) -> String {
        let (scheme, canonical) = match self {
            // #6548: `region` is deliberately absent from the canonical form.
            Self::S3 {
                bucket,
                prefix,
                profile,
                role_arn,
                ..
            } => {
                // The suffix is appended only when an identity is pinned, so a
                // destination written before #6657 keeps the namespace its
                // existing cache directory already carries.
                let identity = match (profile.as_deref(), role_arn.as_deref()) {
                    (None, None) => String::new(),
                    (p, r) => format!(
                        "#profile={}&role_arn={}",
                        p.unwrap_or_default(),
                        r.unwrap_or_default()
                    ),
                };
                ("s3", format!("s3://{bucket}/{prefix}{identity}"))
            }
            Self::File { path } => ("file", format!("file://{}", path.display())),
        };
        // `hex_digest` is a SHA-256 in hex, so it is always 64 characters.
        let digest = hex_digest(canonical.as_bytes());
        format!("{scheme}-{}", &digest[..16])
    }

    /// The scheme this destination was parsed from.
    pub fn scheme(&self) -> DestinationScheme {
        match self {
            Self::S3 { .. } => DestinationScheme::S3,
            Self::File { .. } => DestinationScheme::File,
        }
    }
}

/// The three `s3://` query parameters, before they reach the enum variant.
#[derive(Debug, Default)]
struct S3Params {
    region: Option<String>,
    profile: Option<String>,
    role_arn: Option<String>,
}

/// Extract `region=`, `profile=`, and `role_arn=` from an `s3://` query string.
///
/// Rejects any other parameter rather than ignoring it: a silently-dropped
/// `?reigon=eu-west-1` would send logs to the wrong region with no signal, and
/// a silently-dropped `?porfile=logs-writer` would send them to the wrong
/// ACCOUNT under whatever identity the default chain happened to hold (#6657).
/// A repeated key is rejected for the same reason — picking the last occurrence
/// makes `?profile=a&profile=b` mean `b` with nothing on screen saying so.
fn parse_s3_query(uri: &str, query: &str) -> Result<S3Params, DrainError> {
    let mut params = S3Params::default();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once('=').ok_or_else(|| DrainError::Uri {
            uri: uri.to_string(),
            reason: format!("query parameter `{pair}` has no `=`"),
        })?;
        if value.is_empty() {
            return Err(DrainError::Uri {
                uri: uri.to_string(),
                reason: format!("`{key}=` is empty"),
            });
        }
        let slot = match key {
            "region" => &mut params.region,
            "profile" => &mut params.profile,
            "role_arn" => &mut params.role_arn,
            _ => {
                return Err(DrainError::Uri {
                    uri: uri.to_string(),
                    reason: format!(
                        "unknown query parameter `{key}` — only `region`, `profile`, \
                         and `role_arn` are accepted"
                    ),
                });
            }
        };
        if slot.is_some() {
            return Err(DrainError::Uri {
                uri: uri.to_string(),
                reason: format!("query parameter `{key}` is given more than once"),
            });
        }
        *slot = Some(value.to_string());
    }
    if params.region.is_none() && params.profile.is_none() && params.role_arn.is_none() {
        return Err(DrainError::Uri {
            uri: uri.to_string(),
            reason: "query string is present but empty".to_string(),
        });
    }
    Ok(params)
}

/// Strip leading and trailing `/` and collapse the empty case.
fn normalise_prefix(prefix: &str) -> String {
    prefix.trim_matches('/').to_string()
}
