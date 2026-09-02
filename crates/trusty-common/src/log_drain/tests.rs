//! Coverage for the log-drain core (#6533).
//!
//! Every test that needs a destination uses a REAL `file://`
//! [`ObjectStoreDestination`] over a `tempfile::TempDir`, not a mock. A pass
//! here is therefore evidence about the same `put`/`get`/`head`/`list` code the
//! `s3://` destination runs, differing only in transport.
//!
//! `TRUSTY_LOG_DRAIN_S3_SMOKE_URI` opts into one real-S3 round trip; unset, that
//! test prints why it is skipping and passes.

use std::io::Read;
use std::path::{Path, PathBuf};

use super::*;

// ── helpers ─────────────────────────────────────────────────────────────────

/// A `file://` destination rooted in a fresh temp dir.
async fn file_dest(root: &Path) -> ObjectStoreDestination {
    let uri = DestinationUri::parse(&format!("file://{}", root.display()))
        .expect("temp dir path parses as a file:// URI");
    ObjectStoreDestination::connect(&uri)
        .await
        .expect("local destination connects")
}

fn target() -> DrainTarget {
    DrainTarget {
        owner: "bobmatnyc".to_string(),
        project: "trusty-tools".to_string(),
    }
}

/// One source rooted at `root`, matching every `.log` file.
fn source(root: &Path, level_filter: Option<Level>) -> LogSource {
    LogSource {
        crate_name: "trusty-mpm".to_string(),
        root: root.to_path_buf(),
        include: vec!["**/*.log".to_string()],
        level_filter,
    }
}

fn write(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture dir");
    }
    std::fs::write(path, body).expect("fixture write");
}

/// Read an object back through the destination and gunzip it.
async fn read_gunzipped(dest: &dyn LogDestination, key: &str) -> String {
    let raw = dest
        .get(key)
        .await
        .expect("get succeeds")
        .unwrap_or_else(|| panic!("object `{key}` is absent"));
    let mut decoder = flate2::read::GzDecoder::new(&raw[..]);
    let mut text = String::new();
    decoder
        .read_to_string(&mut text)
        .expect("body is valid gzip");
    text
}

/// Force a file's mtime forward without changing its bytes.
fn touch_forward(path: &Path) {
    let body = std::fs::read(path).expect("read for touch");
    // A rewrite of identical bytes moves mtime while leaving content identical —
    // exactly the case `manifest_sha_beats_mtime` is about.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(path, body).expect("rewrite for touch");
}

// ── DestinationUri: the table ───────────────────────────────────────────────

/// An `s3://` destination with no identity pinned — the shape of most cases.
fn s3(bucket: &str, prefix: &str, region: Option<&str>) -> DestinationUri {
    DestinationUri::S3 {
        bucket: bucket.into(),
        prefix: prefix.into(),
        region: region.map(str::to_string),
        profile: None,
        role_arn: None,
    }
}

#[test]
fn uri_table_accepts() {
    let cases: &[(&str, DestinationUri)] = &[
        ("s3://my-bucket", s3("my-bucket", "", None)),
        ("s3://my-bucket/", s3("my-bucket", "", None)),
        ("s3://my-bucket/logs", s3("my-bucket", "logs", None)),
        (
            "s3://my-bucket/logs/nested/",
            s3("my-bucket", "logs/nested", None),
        ),
        ("  s3://my-bucket/logs  ", s3("my-bucket", "logs", None)),
        ("S3://my-bucket/logs", s3("my-bucket", "logs", None)),
        (
            "file:///var/log/trusty",
            DestinationUri::File {
                path: PathBuf::from("/var/log/trusty"),
            },
        ),
        (
            "file:///var/log/trusty/",
            DestinationUri::File {
                path: PathBuf::from("/var/log/trusty"),
            },
        ),
    ];

    for (input, expected) in cases {
        let parsed = DestinationUri::parse(input)
            .unwrap_or_else(|e| panic!("`{input}` should parse, got {e}"));
        assert_eq!(&parsed, expected, "parsing `{input}`");
    }
}

#[test]
fn uri_region_override() {
    let parsed = DestinationUri::parse("s3://bucket/logs?region=us-west-2").expect("parses");
    assert_eq!(parsed, s3("bucket", "logs", Some("us-west-2")));
}

#[test]
fn uri_accepts_profile_and_role_arn() {
    // #6657: a per-source destination has to be able to name its own identity,
    // or every destination in one daemon signs with whatever the process-wide
    // default chain happened to resolve.
    /// One URI and the three query values it must parse to.
    struct Case {
        uri: &'static str,
        region: Option<&'static str>,
        profile: Option<&'static str>,
        role_arn: Option<&'static str>,
    }

    let arn = "arn:aws:iam::141377423791:role/log-drain-writer";
    let cases = [
        Case {
            uri: "s3://b/p?profile=logs-writer",
            region: None,
            profile: Some("logs-writer"),
            role_arn: None,
        },
        Case {
            uri: "s3://b/p?role_arn=arn:aws:iam::141377423791:role/log-drain-writer",
            region: None,
            profile: None,
            role_arn: Some(arn),
        },
        // All three, in an order that is not the parser's own.
        Case {
            uri: "s3://b/p?role_arn=arn:aws:iam::141377423791:role/log-drain-writer\
                  &profile=logs-writer&region=us-east-1",
            region: Some("us-east-1"),
            profile: Some("logs-writer"),
            role_arn: Some(arn),
        },
    ];

    for case in cases {
        let parsed = DestinationUri::parse(case.uri)
            .unwrap_or_else(|e| panic!("`{}` should parse, got {e}", case.uri));
        assert_eq!(
            parsed,
            DestinationUri::S3 {
                bucket: "b".into(),
                prefix: "p".into(),
                region: case.region.map(str::to_string),
                profile: case.profile.map(str::to_string),
                role_arn: case.role_arn.map(str::to_string),
            },
            "parsing `{}`",
            case.uri
        );
    }
}

#[test]
fn cache_namespace_separates_destinations() {
    let a = DestinationUri::parse("s3://bucket-a/logs").expect("bucket-a");
    let b = DestinationUri::parse("s3://bucket-b/logs").expect("bucket-b");
    let other_prefix = DestinationUri::parse("s3://bucket-a/other").expect("other prefix");
    let local = DestinationUri::parse("file:///tmp/drain-a").expect("file");

    assert_ne!(a.cache_namespace(), b.cache_namespace(), "bucket");
    assert_ne!(
        a.cache_namespace(),
        other_prefix.cache_namespace(),
        "prefix"
    );
    assert_ne!(a.cache_namespace(), local.cache_namespace(), "scheme");

    // A cache directory is named with this, so it is one segment and it is
    // recognisable to an operator reading `ls`.
    assert!(a.cache_namespace().starts_with("s3-"));
    assert!(local.cache_namespace().starts_with("file-"));
    assert!(!a.cache_namespace().contains('/'));
    assert!(!local.cache_namespace().contains('/'));

    // Unstable between calls would mean the cache is written and never read.
    let a_again = DestinationUri::parse("s3://bucket-a/logs").expect("bucket-a again");
    assert_eq!(a.cache_namespace(), a_again.cache_namespace());
}

#[test]
fn cache_namespace_separates_identities() {
    // #6657: two identities against one bucket can see different objects, so a
    // skip decision made under one must not be read back under the other. The
    // asymmetry with `?region=` below is deliberate — a region change cannot
    // change what a bucket holds, an identity change can change what it shows.
    let plain = DestinationUri::parse("s3://bucket-a/logs").expect("plain");
    let profiled = DestinationUri::parse("s3://bucket-a/logs?profile=alpha").expect("profiled");
    let other_profile = DestinationUri::parse("s3://bucket-a/logs?profile=beta").expect("beta");
    let roled =
        DestinationUri::parse("s3://bucket-a/logs?role_arn=arn:aws:iam::1:role/r").expect("roled");

    assert_ne!(plain.cache_namespace(), profiled.cache_namespace());
    assert_ne!(profiled.cache_namespace(), other_profile.cache_namespace());
    assert_ne!(plain.cache_namespace(), roled.cache_namespace());
    assert_ne!(profiled.cache_namespace(), roled.cache_namespace());

    // A destination that pins no identity keeps the namespace it had before
    // #6657, so upgrading does not orphan an existing cache directory.
    let regioned = DestinationUri::parse("s3://bucket-a/logs?region=eu-west-1").expect("regioned");
    assert_eq!(plain.cache_namespace(), regioned.cache_namespace());
}

#[test]
fn cache_namespace_ignores_the_region_override() {
    // `?region=` changes which endpoint serves a bucket, never which objects it
    // holds, so adding one must not orphan a cache that is still valid (#6548).
    let plain = DestinationUri::parse("s3://bucket-a/logs").expect("plain");
    let regioned = DestinationUri::parse("s3://bucket-a/logs?region=eu-west-1").expect("regioned");
    assert_eq!(plain.cache_namespace(), regioned.cache_namespace());
}

#[test]
fn uri_table_rejects() {
    // Each case must fail with DrainError::Uri, not a scheme error.
    let cases = [
        ("s3:/bucket", "no `://` separator"),
        ("bucket/logs", "no `://` separator"),
        ("", "no `://` separator"),
        ("s3:///logs", "empty bucket"),
        ("file://relative/path", "non-absolute file path"),
        ("file:///", "filesystem root"),
        ("file:///tmp/x?region=eu", "query on file://"),
        ("s3://bucket/logs?", "empty query"),
        ("s3://bucket/logs?region=", "empty region value"),
        ("s3://bucket/logs?reigon=eu-west-1", "misspelled parameter"),
        ("s3://bucket/logs?region", "parameter with no `=`"),
        // #6657: the new keys are rejected the same way when misspelled or
        // empty, and a repeated key never silently resolves to the last one.
        ("s3://bucket/logs?porfile=writer", "misspelled profile"),
        ("s3://bucket/logs?profile=", "empty profile value"),
        ("s3://bucket/logs?role_arn=", "empty role_arn value"),
        ("s3://bucket/logs?rolearn=arn:x", "misspelled role_arn"),
        ("s3://bucket/logs?profile=a&profile=b", "repeated parameter"),
    ];

    for (input, why) in cases {
        match DestinationUri::parse(input) {
            Err(DrainError::Uri { .. }) => {}
            other => panic!("`{input}` ({why}) should be DrainError::Uri, got {other:?}"),
        }
    }
}

#[test]
fn uri_reserved_schemes() {
    // gs:// and az:// are RECOGNISED and then refused, so the message names
    // what is supported instead of reading as a syntax error.
    for input in ["gs://bucket/logs", "az://container/logs", "https://example"] {
        match DestinationUri::parse(input) {
            Err(DrainError::UnsupportedScheme { .. }) => {}
            other => panic!("`{input}` should be UnsupportedScheme, got {other:?}"),
        }
    }

    let message = DestinationUri::parse("gs://bucket/logs")
        .expect_err("gs:// is refused")
        .to_string();
    assert!(
        message.contains("s3://") && message.contains("file://"),
        "the refusal must name what IS supported, got: {message}"
    );
}

// ── key layout ──────────────────────────────────────────────────────────────

#[test]
fn key_layout_shape() {
    // #6657: `<owner>/<project>/<relative log path>`, with no session segment
    // and no `logs/` interlayer.
    let t = target();
    assert_eq!(t.key_prefix(), "bobmatnyc/trusty-tools");
    assert_eq!(
        t.object_key("trusty-mpm/daemon.log"),
        "bobmatnyc/trusty-tools/trusty-mpm/daemon.log"
    );
    assert_eq!(
        t.manifest_key(),
        "bobmatnyc/trusty-tools/.drain-manifest.json"
    );
}

#[test]
fn identity_refusal_is_fail_closed() {
    for (owner, project, field) in [
        ("", "proj", "owner"),
        ("   ", "proj", "owner"),
        ("bob", "", "project"),
        ("bob", "  ", "project"),
    ] {
        let t = DrainTarget {
            owner: owner.to_string(),
            project: project.to_string(),
        };
        match t.validate() {
            Err(DrainError::MissingIdentity { field: got }) => assert_eq!(got, field),
            other => panic!("`{owner}`/`{project}` should refuse, got {other:?}"),
        }
    }
}

// ── destination round trip ──────────────────────────────────────────────────

#[tokio::test]
async fn destination_roundtrip() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(root.path()).await;

    assert!(
        dest.head("absent/key")
            .await
            .expect("head succeeds")
            .is_none(),
        "a missing object must be Ok(None), never an error"
    );
    assert!(
        dest.get("absent/key")
            .await
            .expect("get succeeds")
            .is_none()
    );

    let body = bytes::Bytes::from_static(b"hello drain");
    dest.put("a/b.txt", body.clone(), PutMeta::gzipped_text())
        .await
        .expect("put succeeds");

    let meta = dest
        .head("a/b.txt")
        .await
        .expect("head succeeds")
        .expect("object is present after put");
    assert_eq!(meta.size, body.len() as u64);
    assert_eq!(meta.key, "a/b.txt");

    assert_eq!(
        dest.get("a/b.txt").await.expect("get succeeds").as_deref(),
        Some(&body[..])
    );
}

#[tokio::test]
async fn destination_list_is_bounded_and_prefix_scoped() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(root.path()).await;

    for i in 0..5 {
        dest.put(
            &format!("keep/{i}.txt"),
            bytes::Bytes::from_static(b"x"),
            PutMeta::default(),
        )
        .await
        .expect("put");
    }
    dest.put(
        "other/z.txt",
        bytes::Bytes::from_static(b"x"),
        PutMeta::default(),
    )
    .await
    .expect("put");

    let listed = dest.list("keep").await.expect("list succeeds");
    assert_eq!(listed.len(), 5, "list is scoped to its prefix");
    assert!(listed.iter().all(|m| m.key.starts_with("keep/")));
    assert!(
        listed.len() <= LIST_LIMIT,
        "list must never exceed its documented cap"
    );
}

// ── #6657: per-destination S3 identity ──────────────────────────────────────

/// Two profiles with distinct static keys, supplied in memory.
///
/// Nothing is written to disk and no env var is touched, so this cannot pick up
/// the developer's own `~/.aws` files or collide with a concurrent test.
#[allow(deprecated)]
fn fixture_profiles() -> super::destination::ProfileFiles {
    use aws_config::profile::profile_file::ProfileFileKind;
    super::destination::ProfileFiles::builder()
        .with_contents(
            ProfileFileKind::Credentials,
            "[alpha]\n\
             aws_access_key_id = AKIAALPHAALPHAALPHA\n\
             aws_secret_access_key = alpha-secret\n\
             \n\
             [beta]\n\
             aws_access_key_id = AKIABETABETABETABET\n\
             aws_secret_access_key = beta-secret\n\
             \n\
             [regionless]\n\
             aws_access_key_id = AKIANOREGIONNOREGIO\n\
             aws_secret_access_key = regionless-secret\n",
        )
        .with_contents(
            ProfileFileKind::Config,
            "[profile alpha]\nregion = us-east-1\n\n[profile beta]\nregion = eu-west-1\n",
        )
        .build()
}

/// The role every `?role_arn=` fixture assumes. Never contacted: the provider
/// is lazy, so building one reaches no STS endpoint.
const FIXTURE_ROLE_ARN: &str = "arn:aws:iam::123456789012:role/log-drain";

/// Resolve one destination's identity against the in-memory profile fixture.
async fn resolve_fixture_auth(
    label: &str,
    region: Option<&str>,
    profile: Option<&str>,
) -> Result<super::destination::S3Auth, DrainError> {
    resolve_fixture_auth_with_role(label, region, profile, None).await
}

/// The same, with an explicit `?role_arn=`.
///
/// Every caller also pins a `profile`. A role with no profile falls through to
/// the AWS default chain, which reads the developer's own environment and
/// `~/.aws` files — the one thing this fixture exists to avoid.
async fn resolve_fixture_auth_with_role(
    label: &str,
    region: Option<&str>,
    profile: Option<&str>,
    role_arn: Option<&str>,
) -> Result<super::destination::S3Auth, DrainError> {
    let files = fixture_profiles();
    super::destination::resolve_s3_auth(
        label,
        super::destination::S3AuthRequest {
            region,
            profile,
            role_arn,
            profile_files: Some(&files),
        },
    )
    .await
}

#[tokio::test]
async fn two_profiles_resolve_to_different_identities() {
    // #6657: the whole point of a per-source `?profile=` is that two
    // destinations in ONE daemon sign with two different sets of credentials.
    // Proving it needs no bucket and no network — the credentials the provider
    // hands back are the observable.
    use aws_credential_types::provider::ProvideCredentials;

    let alpha = resolve_fixture_auth("s3://a/logs", None, Some("alpha"))
        .await
        .expect("alpha resolves");
    let beta = resolve_fixture_auth("s3://b/logs", None, Some("beta"))
        .await
        .expect("beta resolves");

    let alpha_key = alpha
        .provider
        .provide_credentials()
        .await
        .expect("alpha credentials")
        .access_key_id()
        .to_string();
    let beta_key = beta
        .provider
        .provide_credentials()
        .await
        .expect("beta credentials")
        .access_key_id()
        .to_string();

    assert_eq!(alpha_key, "AKIAALPHAALPHAALPHA");
    assert_eq!(beta_key, "AKIABETABETABETABET");
    assert_ne!(
        alpha_key, beta_key,
        "two profiles must not collapse onto one identity"
    );

    // Each profile's own `region` is used, so a two-account layout needs no
    // `?region=` on either URI.
    assert_eq!(alpha.region, "us-east-1");
    assert_eq!(beta.region, "eu-west-1");
}

#[tokio::test]
async fn a_uri_region_overrides_the_profile_region() {
    let resolved = resolve_fixture_auth("s3://a/logs", Some("ap-south-1"), Some("alpha"))
        .await
        .expect("resolves");
    assert_eq!(resolved.region, "ap-south-1");
}

#[tokio::test]
async fn a_profile_without_a_region_is_refused() {
    // Signing for the wrong region reaches a bucket that is not there, so there
    // is no literal to fall back to — the refusal names both levers.
    let err = resolve_fixture_auth("s3://a/logs", None, Some("regionless"))
        .await
        .expect_err("a profile with no region and no `?region=` cannot be signed for");
    match err {
        DrainError::Credentials { ref uri, .. } => assert_eq!(uri, "s3://a/logs"),
        other => panic!("expected DrainError::Credentials, got {other:?}"),
    }
    let message = err.to_string();
    assert!(
        message.contains("?region=") && message.contains("profile"),
        "the refusal must name what to set, got: {message}"
    );
}

#[tokio::test]
async fn a_role_arn_resolves_to_an_assumed_role_identity() {
    // #6657: `?role_arn=` is how one daemon writes into an account it holds no
    // long-lived keys for. The provider the branch builds is the observable —
    // reading credentials from it would call STS, and the design is that it
    // does not until the first upload.
    let resolved =
        resolve_fixture_auth_with_role("s3://a/logs", None, Some("alpha"), Some(FIXTURE_ROLE_ARN))
            .await
            .expect("a role over a profile resolves");

    // The region the STS client is pinned to comes off the same ladder an
    // ordinary destination uses — here, the profile's own.
    assert_eq!(resolved.region, "us-east-1");
    let provider = format!("{:?}", resolved.provider);
    assert!(
        provider.contains("AssumeRoleProvider"),
        "the role branch must wrap the base identity, got: {provider}"
    );
}

#[tokio::test]
async fn a_profile_and_a_role_arn_do_not_collapse_onto_the_profile() {
    // The two knobs compose: the profile signs the AssumeRole, and the assumed
    // role is what reaches S3. Returning the base provider unwrapped would
    // still resolve, still carry the right region, and write with the wrong
    // identity — so the with/without comparison is the assertion, not the Ok.
    let plain = resolve_fixture_auth("s3://a/logs", None, Some("alpha"))
        .await
        .expect("the profile alone resolves");
    let assumed = resolve_fixture_auth_with_role(
        "s3://a/logs",
        Some("ap-south-1"),
        Some("alpha"),
        Some(FIXTURE_ROLE_ARN),
    )
    .await
    .expect("the profile plus a role resolves");

    let plain_provider = format!("{:?}", plain.provider);
    let assumed_provider = format!("{:?}", assumed.provider);
    assert!(
        !plain_provider.contains("AssumeRoleProvider"),
        "no `?role_arn=` was given, got: {plain_provider}"
    );
    assert!(
        assumed_provider.contains("AssumeRoleProvider"),
        "the same profile plus a role must not resolve to the bare profile \
         identity, got: {assumed_provider}"
    );

    // `?region=` still wins over the profile's own with a role in play, so the
    // STS client is pinned to the region the operator asked for.
    assert_eq!(plain.region, "us-east-1");
    assert_eq!(assumed.region, "ap-south-1");
}

// ── collector: level filtering ──────────────────────────────────────────────

/// A fixture in `tracing_subscriber::fmt`'s default line shape.
const TRACING_FIXTURE: &str = concat!(
    "2026-09-01T14:12:32.273982Z DEBUG tm::commands: dropped debug detail\n",
    "2026-09-01T14:12:32.283982Z  INFO tm::daemon: daemon started\n",
    "2026-09-01T14:12:32.293982Z TRACE tm::inner: dropped trace detail\n",
    "2026-09-01T14:12:32.303982Z  WARN tm::daemon: port already bound\n",
    "2026-09-01T14:12:32.313982Z ERROR tm::daemon: bind failed\n",
    "    caused by: address in use\n",
);

#[test]
fn collect_filters_below_info() {
    let root = tempfile::tempdir().expect("tempdir");
    write(&root.path().join("daemon.log"), TRACING_FIXTURE);

    let collected = collect(
        &[source(root.path(), Some(Level::Info))],
        &[],
        CollectLimits::default(),
    )
    .expect("collect succeeds");

    assert_eq!(collected.files.len(), 1);
    let mut decoder = flate2::read::GzDecoder::new(&collected.files[0].body[..]);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gunzip");

    assert!(
        !text.contains("dropped debug detail"),
        "DEBUG must be dropped"
    );
    assert!(
        !text.contains("dropped trace detail"),
        "TRACE must be dropped"
    );
    assert!(text.contains("daemon started"), "INFO must be kept");
    assert!(text.contains("port already bound"), "WARN must be kept");
    assert!(text.contains("bind failed"), "ERROR must be kept");
    assert!(
        text.contains("address in use"),
        "a continuation line inherits its parent's disposition"
    );
}

#[test]
fn collect_drops_continuation_of_a_dropped_line() {
    let root = tempfile::tempdir().expect("tempdir");
    write(
        &root.path().join("daemon.log"),
        "2026-09-01T14:12:32.2Z DEBUG tm: noisy\n        continuation of noise\n\
         2026-09-01T14:12:32.3Z  INFO tm: kept\n",
    );

    let collected = collect(
        &[source(root.path(), Some(Level::Info))],
        &[],
        CollectLimits::default(),
    )
    .expect("collect");
    let mut decoder = flate2::read::GzDecoder::new(&collected.files[0].body[..]);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gunzip");

    assert!(!text.contains("continuation of noise"));
    assert!(text.contains("kept"));
}

#[test]
fn collect_passes_through_non_tracing() {
    let root = tempfile::tempdir().expect("tempdir");
    let plain = "just a line\nand another\nno levels here at all\n";
    write(&root.path().join("plain.log"), plain);

    let collected = collect(
        &[source(root.path(), Some(Level::Info))],
        &[],
        CollectLimits::default(),
    )
    .expect("collect");

    let mut decoder = flate2::read::GzDecoder::new(&collected.files[0].body[..]);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gunzip");

    assert_eq!(
        text, plain,
        "a file with no recognisable level line is not tracing output and must \
         pass through verbatim rather than filter to nothing"
    );
}

#[test]
fn collect_recognises_ansi_coloured_levels() {
    let root = tempfile::tempdir().expect("tempdir");
    write(
        &root.path().join("colour.log"),
        "\u{1b}[2m2026-09-01T14:12:32.2Z\u{1b}[0m \u{1b}[34mDEBUG\u{1b}[0m tm: noise\n\
         \u{1b}[2m2026-09-01T14:12:32.3Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m tm: kept\n",
    );

    let collected = collect(
        &[source(root.path(), Some(Level::Info))],
        &[],
        CollectLimits::default(),
    )
    .expect("collect");
    let mut decoder = flate2::read::GzDecoder::new(&collected.files[0].body[..]);
    let mut text = String::new();
    decoder.read_to_string(&mut text).expect("gunzip");

    assert!(
        !text.contains("noise"),
        "a colourised DEBUG line is still DEBUG"
    );
    assert!(text.contains("kept"));
}

// ── collector: size ceiling ─────────────────────────────────────────────────

#[test]
fn collect_skips_oversize() {
    let root = tempfile::tempdir().expect("tempdir");
    write(&root.path().join("big.log"), &"x".repeat(4096));
    write(&root.path().join("small.log"), "tiny\n");

    let collected = collect(
        &[source(root.path(), None)],
        &[],
        CollectLimits::new(1024, DEFAULT_MAX_WIRE_BYTES),
    )
    .expect("collect");

    assert_eq!(collected.files.len(), 1, "only the small file is collected");
    assert_eq!(collected.files[0].relative_key, "trusty-mpm/small.log");
    assert_eq!(collected.oversize.len(), 1);
    assert_eq!(collected.oversize[0].size, 4096);
    assert!(collected.oversize[0].path.ends_with("big.log"));
}

// ── collector: the scrub ────────────────────────────────────────────────────

#[tokio::test]
async fn collect_scrubs_secrets_before_they_reach_the_destination() {
    let root = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");

    // Well over `scrub_secrets`' minimum needle length, so it is actually scrubbed.
    let secret = "sk-planted-secret-value-0123456789";
    write(
        &root.path().join("daemon.log"),
        &format!("2026-09-01T14:12:32.2Z  INFO tm: token={secret} ok\n"),
    );

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path()).with_secrets(vec![secret.to_string()]);
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(root.path(), Some(Level::Info))])
        .await
        .expect("run_once succeeds");
    assert_eq!(report.uploaded, 1);

    let key = t.object_key("trusty-mpm/daemon.log");
    let text = read_gunzipped(&dest, &key).await;

    assert!(
        !text.contains(secret),
        "the planted secret reached the destination: {text}"
    );
    assert!(
        text.contains("[REDACTED]"),
        "the scrub must leave its marker"
    );
    assert!(text.contains("token="), "surrounding text must survive");

    // Belt and braces: the secret must not be in the raw bytes on disk either.
    let on_disk = std::fs::read(dest_root.path().join(&key)).expect("object file exists");
    assert!(
        !String::from_utf8_lossy(&on_disk).contains(secret),
        "the secret survived in the stored bytes"
    );
}

// ── manifest ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn manifest_roundtrip() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    let mut manifest = DrainManifest::default();
    assert_eq!(manifest.version, MANIFEST_VERSION);
    manifest.record(ManifestEntry {
        relative_file: "trusty-mpm/a.log".into(),
        size: 10,
        mtime_unix: 1_700_000_000,
        sha256: "abc".into(),
        uploaded_at: "2026-09-01T00:00:00Z".into(),
    });

    manifest
        .save(&dest, state.path(), "k/manifest.json", "bob/sess")
        .await
        .expect("save");

    let loaded = DrainManifest::load(&dest, state.path(), "k/manifest.json", "bob/sess")
        .await
        .expect("load");
    assert_eq!(loaded.entries, manifest.entries);
}

#[tokio::test]
async fn manifest_remote_wins_over_local_cache() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    // Local cache claims a file was uploaded; the remote manifest says otherwise.
    let mut stale = DrainManifest::default();
    stale.record(ManifestEntry {
        relative_file: "trusty-mpm/ghost.log".into(),
        size: 1,
        mtime_unix: 1,
        sha256: "stale".into(),
        uploaded_at: "2026-01-01T00:00:00Z".into(),
    });
    let cache_path = DrainManifest::cache_path(state.path(), &dest, "bob/sess");
    std::fs::create_dir_all(cache_path.parent().expect("parent")).expect("cache dir");
    std::fs::write(&cache_path, serde_json::to_vec(&stale).expect("encode")).expect("cache write");

    let remote = DrainManifest::default();
    remote
        .save(&dest, state.path(), "k/manifest.json", "unrelated")
        .await
        .expect("save remote");

    let loaded = DrainManifest::load(&dest, state.path(), "k/manifest.json", "bob/sess")
        .await
        .expect("load");
    assert!(
        loaded.entries.is_empty(),
        "the remote copy is authoritative; the stale cache entry must not survive"
    );

    let refreshed: DrainManifest =
        serde_json::from_slice(&std::fs::read(&cache_path).expect("cache")).expect("decode");
    assert!(
        refreshed.entries.is_empty(),
        "loading must rewrite the cache from the authoritative remote copy"
    );
}

#[tokio::test]
async fn manifest_corrupt_remote_falls_back_to_cache() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    dest.put(
        "k/manifest.json",
        bytes::Bytes::from_static(b"{ not json"),
        PutMeta::default(),
    )
    .await
    .expect("put corrupt manifest");

    // An undecodable remote manifest is treated as ABSENT, never as an error —
    // re-uploading is safer than skipping something that was never written.
    let loaded = DrainManifest::load(&dest, state.path(), "k/manifest.json", "bob/sess")
        .await
        .expect("a corrupt manifest must not fail the load");
    assert!(loaded.entries.is_empty());
}

#[test]
fn manifest_stat_fast_path_and_sha_tiebreak() {
    let mut manifest = DrainManifest::default();
    manifest.record(ManifestEntry {
        relative_file: "c/a.log".into(),
        size: 100,
        mtime_unix: 42,
        sha256: "deadbeef".into(),
        uploaded_at: "2026-09-01T00:00:00Z".into(),
    });

    assert_eq!(
        manifest.decide("c/a.log", 100, 42),
        StatDecision::SkipUnchanged
    );
    assert_eq!(manifest.decide("c/a.log", 101, 42), StatDecision::NeedsHash);
    assert_eq!(manifest.decide("c/a.log", 100, 43), StatDecision::NeedsHash);
    assert_eq!(
        manifest.decide("c/absent.log", 100, 42),
        StatDecision::NeedsHash
    );

    assert!(manifest.digest_matches("c/a.log", "deadbeef"));
    assert!(!manifest.digest_matches("c/a.log", "cafe"));
    assert!(!manifest.digest_matches("c/absent.log", "deadbeef"));
}

// ── run_once ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn run_once_end_to_end() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");

    write(&logs.path().join("daemon.log"), TRACING_FIXTURE);
    write(&logs.path().join("nested/worker.log"), TRACING_FIXTURE);
    write(&logs.path().join("ignored.txt"), "not a .log file\n");

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), Some(Level::Info))])
        .await
        .expect("run_once");

    assert_eq!(report.uploaded, 2, "both .log files, not the .txt");
    assert_eq!(report.skipped_unchanged, 0);
    assert_eq!(report.skipped_too_large, 0);
    assert!(
        report.errors.is_empty(),
        "unexpected errors: {:?}",
        report.errors
    );
    assert!(report.bytes_plain > 0);
    assert!(report.bytes_wire > 0);

    // The objects landed under the documented key layout.
    for relative in ["trusty-mpm/daemon.log", "trusty-mpm/nested/worker.log"] {
        let key = t.object_key(relative);
        assert!(
            dest.head(&key).await.expect("head").is_some(),
            "expected an object at `{key}`"
        );
    }
    // And so did the manifest.
    assert!(
        dest.head(&t.manifest_key()).await.expect("head").is_some(),
        "the manifest must be rewritten after a successful batch"
    );
}

#[tokio::test]
async fn run_once_is_idempotent() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("daemon.log"), TRACING_FIXTURE);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();
    let sources = [source(logs.path(), Some(Level::Info))];

    let first = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("first run");
    assert_eq!(first.uploaded, 1);

    let second = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("second run");
    assert_eq!(second.uploaded, 0, "nothing changed, so nothing re-uploads");
    assert_eq!(second.skipped_unchanged, 1);
    assert_eq!(second.bytes_wire, 0);
    assert_eq!(
        second.manifest_spot_check_missing, 0,
        "the manifest describes a file that really is there"
    );
}

/// Every key under `prefix`, with its size, sorted — a stable snapshot of what
/// a destination holds.
async fn contents(dest: &dyn LogDestination, prefix: &str) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = dest
        .list(prefix)
        .await
        .expect("list")
        .into_iter()
        .map(|m| (m.key, m.size))
        .collect();
    out.sort();
    out
}

#[tokio::test]
async fn run_once_reuploads_when_the_destination_changes() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_a_root = tempfile::tempdir().expect("tempdir");
    let dest_b_root = tempfile::tempdir().expect("tempdir");
    // ONE state dir across both runs: the operator changed
    // `log_drain.destination`, not their machine.
    let state = tempfile::tempdir().expect("tempdir");

    write(&logs.path().join("daemon.log"), TRACING_FIXTURE);
    write(&logs.path().join("nested/worker.log"), TRACING_FIXTURE);

    let cfg = DrainConfig::new(state.path());
    let t = target();
    let sources = [source(logs.path(), Some(Level::Info))];

    let dest_a = file_dest(dest_a_root.path()).await;
    let first = run_once(&cfg, &dest_a, &t, &sources)
        .await
        .expect("run against A");
    assert_eq!(first.uploaded, 2);
    let a_before = contents(&dest_a, &t.key_prefix()).await;

    // Same identity, same state dir, a destination that holds nothing. B has no
    // manifest of its own, so before #6548 the load fell back to the cache
    // written for A and classified every file SkipUnchanged — the files never
    // arrived, and B's manifest then claimed they had.
    let dest_b = file_dest(dest_b_root.path()).await;
    let second = run_once(&cfg, &dest_b, &t, &sources)
        .await
        .expect("run against B");

    assert_eq!(
        second.uploaded, 2,
        "a fresh destination holds none of these files yet"
    );
    assert_eq!(
        second.skipped_unchanged, 0,
        "a skip decision made for A must decide nothing for B"
    );

    for relative in ["trusty-mpm/daemon.log", "trusty-mpm/nested/worker.log"] {
        let key = t.object_key(relative);
        assert!(
            dest_b.head(&key).await.expect("head B").is_some(),
            "expected an object at `{key}` under B"
        );
    }
    assert!(
        dest_b
            .head(&t.manifest_key())
            .await
            .expect("head B")
            .is_some(),
        "B gets its own manifest"
    );

    assert_eq!(
        a_before,
        contents(&dest_a, &t.key_prefix()).await,
        "a run aimed at B must not write to A"
    );
}

#[tokio::test]
async fn run_once_spot_checks_a_lying_remote_manifest() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("daemon.log"), TRACING_FIXTURE);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();
    let sources = [source(logs.path(), Some(Level::Info))];

    run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("first run");

    // Exactly what #6548 left in the wild: a remote manifest naming an object
    // this destination never received. One entry, so the sample is determined.
    let mut lying = DrainManifest::default();
    lying.record(ManifestEntry {
        relative_file: "trusty-mpm/never-uploaded.log".into(),
        size: 1,
        mtime_unix: 1,
        sha256: "ghost".into(),
        uploaded_at: "2026-01-01T00:00:00Z".into(),
    });
    dest.put(
        &t.manifest_key(),
        bytes::Bytes::from(serde_json::to_vec(&lying).expect("encode")),
        PutMeta::default(),
    )
    .await
    .expect("put the lying manifest");

    let report = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("second run");
    assert_eq!(
        report.manifest_spot_check_missing, 1,
        "the sampled entry names an object the destination does not have"
    );
    assert_eq!(
        report.uploaded, 1,
        "detection only — the run still uploads what the lying manifest omits"
    );
}

#[tokio::test]
async fn run_once_reuploads_a_mutated_file() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let log = logs.path().join("daemon.log");
    write(&log, TRACING_FIXTURE);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();
    let sources = [source(logs.path(), Some(Level::Info))];

    run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("first run");

    write(
        &log,
        &format!("{TRACING_FIXTURE}2026-09-01T15:00:00.0Z  INFO tm: a new line\n"),
    );

    let second = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("second run");
    assert_eq!(second.uploaded, 1, "changed bytes must re-upload");
    assert_eq!(second.skipped_unchanged, 0);

    let text = read_gunzipped(&dest, &t.object_key("trusty-mpm/daemon.log")).await;
    assert!(
        text.contains("a new line"),
        "the destination holds the new body"
    );
}

#[tokio::test]
async fn run_once_sha_beats_a_moved_mtime() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let log = logs.path().join("daemon.log");
    write(&log, TRACING_FIXTURE);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();
    let sources = [source(logs.path(), Some(Level::Info))];

    run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("first run");
    let recorded_before = manifest_mtime(&dest, &t).await;

    // Same bytes, later mtime. SHA-256 wins: no re-upload.
    touch_forward(&log);

    // Guard against a vacuous pass: if the rewrite did not actually move the
    // mtime, `decide` would take the stat-only fast path and the sha tiebreak
    // below would never run.
    let on_disk_mtime = std::fs::metadata(&log)
        .expect("stat")
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs() as i64;
    assert!(
        on_disk_mtime > recorded_before,
        "fixture is broken: mtime did not move ({on_disk_mtime} vs {recorded_before}), \
         so this test would pass without exercising the sha tiebreak"
    );

    let second = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("second run");
    assert_eq!(
        second.uploaded, 0,
        "identical bytes must not re-upload just because mtime moved"
    );
    assert_eq!(second.skipped_unchanged, 1);

    // The sha tiebreak refreshed the entry's mtime — that refresh is what makes
    // the NEXT run cheap, and it only happens on the sha path.
    let recorded_after = manifest_mtime(&dest, &t).await;
    assert_eq!(
        recorded_after, on_disk_mtime,
        "the sha tiebreak must refresh the recorded mtime so the next run takes \
         the stat-only fast path"
    );

    let third = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("third run");
    assert_eq!(third.uploaded, 0);
    assert_eq!(third.skipped_unchanged, 1);
}

/// The mtime the destination's manifest currently records for `daemon.log`.
async fn manifest_mtime(dest: &dyn LogDestination, t: &DrainTarget) -> i64 {
    let raw = dest
        .get(&t.manifest_key())
        .await
        .expect("get manifest")
        .expect("manifest exists");
    let manifest: DrainManifest = serde_json::from_slice(&raw).expect("decode manifest");
    manifest
        .entries
        .iter()
        .find(|e| e.relative_file == "trusty-mpm/daemon.log")
        .expect("entry for the drained file")
        .mtime_unix
}

#[tokio::test]
async fn run_once_counts_oversize_without_uploading() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("big.log"), &"x".repeat(4096));
    write(&logs.path().join("small.log"), "small\n");

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path()).with_max_file_bytes(1024);
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), None)])
        .await
        .expect("run_once");

    assert_eq!(report.uploaded, 1);
    assert_eq!(report.skipped_too_large, 1);
    assert!(
        dest.head(&t.object_key("trusty-mpm/big.log"))
            .await
            .expect("head")
            .is_none(),
        "an oversize file must never be uploaded, not even truncated"
    );
}

// ── #6547: streaming, and skip decisions made once ──────────────────────────

/// Read the destination's own manifest object.
async fn remote_manifest(dest: &dyn LogDestination, t: &DrainTarget) -> DrainManifest {
    let raw = dest
        .get(&t.manifest_key())
        .await
        .expect("get manifest")
        .expect("manifest object exists");
    serde_json::from_slice(&raw).expect("manifest decodes")
}

/// A tracing-shaped fixture larger than the pipeline's 1 MiB read chunk, with
/// `secret` planted so it straddles the boundary.
///
/// The straddle is the point: a needle split across two reads is found by
/// neither chunk alone, which is the hazard `ScrubCarry` exists for.
fn chunk_straddling_fixture(secret: &str) -> String {
    const CHUNK: usize = 1024 * 1024;
    let line = "2026-09-01T14:12:32.2Z  INFO tm: filler line to reach the chunk boundary\n";
    let mut text = String::with_capacity(CHUNK + 4096);
    // Stop a few bytes short of the boundary so the secret spans it.
    while text.len() + line.len() < CHUNK - (secret.len() / 2) {
        text.push_str(line);
    }
    let pad = CHUNK - (secret.len() / 2) - text.len();
    text.push_str(&"p".repeat(pad));
    text.push_str(secret);
    text.push_str(" tail\n2026-09-01T14:12:33.0Z  INFO tm: after the boundary\n");
    text
}

#[tokio::test]
async fn stream_scrubs_a_secret_straddling_a_chunk_boundary() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");

    let secret = "sk-straddle-0123456789abcdef0123456789abcdef";
    let fixture = chunk_straddling_fixture(secret);
    assert!(
        fixture.len() > 1024 * 1024,
        "the fixture must cross the read chunk"
    );
    write(&logs.path().join("daemon.log"), &fixture);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path()).with_secrets(vec![secret.to_string()]);
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), None)])
        .await
        .expect("run_once");
    assert_eq!(report.uploaded, 1);

    let text = read_gunzipped(&dest, &t.object_key("trusty-mpm/daemon.log")).await;
    assert!(
        !text.contains(secret),
        "a secret split across two read chunks reached the destination"
    );
    assert!(
        text.contains("[REDACTED]"),
        "the scrub must leave its marker"
    );
    assert!(
        text.contains("after the boundary"),
        "text past the boundary must survive"
    );
}

#[tokio::test]
async fn stream_matches_the_buffered_pipeline() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");

    // Levels on both sides of a chunk boundary: the filter's `keeping` state
    // has to survive the split, not reset to its default.
    let mut fixture = chunk_straddling_fixture("sk-unused-needle-value-000000");
    fixture.push_str("2026-09-01T14:12:34.0Z DEBUG tm: dropped after the boundary\n");
    fixture.push_str("        continuation of the dropped line\n");
    fixture.push_str("2026-09-01T14:12:35.0Z ERROR tm: kept after the boundary\n");
    write(&logs.path().join("daemon.log"), &fixture);

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path());
    let t = target();

    run_once(&cfg, &dest, &t, &[source(logs.path(), Some(Level::Info))])
        .await
        .expect("run_once");

    let text = read_gunzipped(&dest, &t.object_key("trusty-mpm/daemon.log")).await;
    assert!(
        !text.contains("dropped after the boundary"),
        "DEBUG past the chunk boundary must still be dropped"
    );
    assert!(
        !text.contains("continuation of the dropped line"),
        "a continuation inherits its parent's disposition across a chunk boundary"
    );
    assert!(
        text.contains("kept after the boundary"),
        "ERROR must survive"
    );
    assert!(
        text.contains("filler line to reach the chunk boundary"),
        "INFO before the boundary must survive"
    );
}

#[tokio::test]
async fn run_once_uploads_a_file_over_the_old_ceiling() {
    // The 64 MiB ceiling is what left 29 days of daemon logs permanently
    // undrained; the streamed pipeline is what let the default move past it.
    const {
        assert!(
            DEFAULT_MAX_FILE_BYTES > 64 * 1024 * 1024,
            "the default source ceiling must clear the pre-#6547 64 MiB"
        );
    }

    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");

    let line = "2026-09-01T14:12:32.2Z  INFO tm: a rotated daemon log line\n";
    let big = line.repeat((65 * 1024 * 1024 / line.len()) + 1);
    assert!(big.len() > 64 * 1024 * 1024, "fixture must clear 64 MiB");
    write(&logs.path().join("trusty-mpm.2026-09-01.log"), &big);

    let dest = file_dest(dest_root.path()).await;
    let t = target();

    let report = run_once(
        &DrainConfig::new(state.path()),
        &dest,
        &t,
        &[source(logs.path(), Some(Level::Info))],
    )
    .await
    .expect("run_once");

    assert_eq!(report.uploaded, 1, "{:?}", report.errors);
    assert_eq!(report.skipped_too_large, 0);
    assert_eq!(report.bytes_plain, big.len() as u64);
    assert!(
        report.bytes_wire < 1024 * 1024,
        "the gzip body is what stays in memory: {} B",
        report.bytes_wire
    );
}

#[tokio::test]
async fn collect_skips_a_body_over_the_wire_cap() {
    let root = tempfile::tempdir().expect("tempdir");
    // Random-ish text so gzip cannot squeeze it under a 64-byte wire cap.
    let body: String = (0..8192u32)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    write(&root.path().join("wide.log"), &body);

    let collected = collect(
        &[source(root.path(), None)],
        &[],
        CollectLimits::new(DEFAULT_MAX_FILE_BYTES, 64),
    )
    .expect("collect");

    assert!(collected.files.is_empty(), "the body must not be truncated");
    assert_eq!(collected.oversize.len(), 1);
    assert_eq!(collected.oversize[0].reason, SkipReason::CompressedTooLarge);
    assert_eq!(collected.oversize[0].reason.limit_name(), "max_wire_bytes");
}

#[tokio::test]
async fn run_once_records_an_oversize_skip_once() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("big.log"), &"x".repeat(4096));

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path()).with_max_file_bytes(1024);
    let t = target();
    let sources = [source(logs.path(), None)];

    let first = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("first pass");
    assert_eq!(first.skipped_too_large, 1);
    assert_eq!(
        first.skips_recorded, 1,
        "the first pass decides, and that is the pass that warns"
    );

    let manifest = remote_manifest(&dest, &t).await;
    assert_eq!(manifest.skips.len(), 1, "the decision must be durable");
    assert_eq!(manifest.skips[0].relative_file, "trusty-mpm/big.log");
    assert_eq!(manifest.skips[0].size, 4096);
    assert_eq!(manifest.skips[0].reason, SkipReason::SourceTooLarge);

    // The cycle that produced 1,276 identical WARNs in 48 hours. `warn!` is
    // gated on the same branch that increments `skips_recorded`, so a zero here
    // is a pass that logged nothing about this file.
    for pass in 0..3 {
        let again = run_once(&cfg, &dest, &t, &sources)
            .await
            .expect("repeat pass");
        assert_eq!(again.skipped_too_large, 1, "pass {pass}");
        assert_eq!(
            again.skips_recorded, 0,
            "pass {pass} re-decided a file that cannot have changed"
        );
    }
}

#[tokio::test]
async fn run_once_re_evaluates_a_skip_when_the_file_changes() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let path = logs.path().join("big.log");
    write(&path, &"x".repeat(4096));

    let dest = file_dest(dest_root.path()).await;
    let cfg = DrainConfig::new(state.path()).with_max_file_bytes(1024);
    let t = target();
    let sources = [source(logs.path(), None)];

    assert_eq!(
        run_once(&cfg, &dest, &t, &sources)
            .await
            .expect("first pass")
            .skips_recorded,
        1
    );

    // A daily log is appended to all day. Its size moved, so the recorded
    // answer no longer describes it and the decision is made again.
    write(&path, &"x".repeat(8192));
    let after_growth = run_once(&cfg, &dest, &t, &sources)
        .await
        .expect("pass after growth");
    assert_eq!(
        after_growth.skips_recorded, 1,
        "a file whose size changed must be re-evaluated"
    );
    assert_eq!(remote_manifest(&dest, &t).await.skips[0].size, 8192);

    // Raising the ceiling makes the same file drainable, and the stale
    // decision must not outlive the upload that contradicts it.
    let raised = DrainConfig::new(state.path()).with_max_file_bytes(1024 * 1024);
    let uploaded = run_once(&raised, &dest, &t, &sources)
        .await
        .expect("pass with a raised ceiling");
    assert_eq!(uploaded.uploaded, 1);
    assert_eq!(uploaded.skipped_too_large, 0);
    assert!(
        remote_manifest(&dest, &t).await.skips.is_empty(),
        "a file that uploaded must carry no skip record"
    );
}

#[tokio::test]
async fn run_once_refuses_an_empty_owner() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    let t = DrainTarget {
        owner: String::new(),
        project: "trusty-tools".to_string(),
    };
    let err = run_once(&DrainConfig::new(state.path()), &dest, &t, &[])
        .await
        .expect_err("an empty owner must be refused");
    assert!(matches!(
        err,
        DrainError::MissingIdentity { field: "owner" }
    ));

    // Fail-closed means NOTHING was written, not even a manifest.
    assert_eq!(
        std::fs::read_dir(dest_root.path())
            .expect("read dest root")
            .count(),
        0,
        "a refused run must leave the destination untouched"
    );
}

#[tokio::test]
async fn run_once_refuses_an_empty_project() {
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    let dest = file_dest(dest_root.path()).await;

    let t = DrainTarget {
        owner: "bobmatnyc".to_string(),
        project: "   ".to_string(),
    };
    let err = run_once(&DrainConfig::new(state.path()), &dest, &t, &[])
        .await
        .expect_err("an empty project must be refused");
    assert!(matches!(
        err,
        DrainError::MissingIdentity { field: "project" }
    ));
}

#[tokio::test]
async fn run_once_collects_per_file_errors_without_aborting() {
    let logs = tempfile::tempdir().expect("tempdir");
    let dest_root = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("a.log"), "one\n");
    write(&logs.path().join("b.log"), "two\n");

    let dest = FailingDestination {
        inner: file_dest(dest_root.path()).await,
        fail_on: "trusty-mpm/a.log".to_string(),
    };
    let cfg = DrainConfig::new(state.path());
    let t = target();

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), None)])
        .await
        .expect("a per-file failure must not fail the run");

    assert_eq!(report.uploaded, 1, "the healthy file still uploads");
    assert_eq!(
        report.errors.len(),
        1,
        "the failure is reported, not raised"
    );
    assert!(report.errors[0].0.ends_with("trusty-mpm/a.log"));
}

/// A destination that fails `put` for one key and delegates everything else.
///
/// Proves [`run_once`] collects a per-file error and CONTINUES the batch —
/// the behaviour that stops one unreadable file stranding every other log.
#[derive(Debug)]
struct FailingDestination {
    inner: ObjectStoreDestination,
    fail_on: String,
}

#[async_trait::async_trait]
impl LogDestination for FailingDestination {
    async fn put(&self, key: &str, body: bytes::Bytes, meta: PutMeta) -> Result<(), DrainError> {
        if key.ends_with(&self.fail_on) {
            return Err(DrainError::Manifest {
                key: key.to_string(),
                reason: "injected failure".to_string(),
            });
        }
        self.inner.put(key, body, meta).await
    }

    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, DrainError> {
        self.inner.head(key).await
    }

    async fn get(&self, key: &str) -> Result<Option<bytes::Bytes>, DrainError> {
        self.inner.get(key).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectMeta>, DrainError> {
        self.inner.list(prefix).await
    }

    fn cache_namespace(&self) -> &str {
        self.inner.cache_namespace()
    }
}

// ── gated real-S3 smoke ─────────────────────────────────────────────────────

/// Env var that opts this run into one real S3 round trip.
const S3_SMOKE_ENV: &str = "TRUSTY_LOG_DRAIN_S3_SMOKE_URI";

/// One real `s3://` upload, read back and verified.
///
/// Why an explicit skip rather than `#[ignore]`: an `#[ignore]`d test is
/// invisible in a normal run, so nobody learns the S3 path exists or how to
/// exercise it. This prints the exact env var to set and passes.
///
/// Set `TRUSTY_LOG_DRAIN_S3_SMOKE_URI=s3://<bucket>/<prefix>` (optionally with
/// `?region=`) and provide AWS credentials the usual way.
#[tokio::test]
async fn s3_smoke() {
    let Ok(uri_str) = std::env::var(S3_SMOKE_ENV) else {
        eprintln!(
            "SKIP s3_smoke: {S3_SMOKE_ENV} is unset. \
             Set it to `s3://<bucket>/<prefix>` with AWS credentials available \
             to exercise the real S3 destination."
        );
        return;
    };

    let uri = DestinationUri::parse(&uri_str).expect("smoke URI parses");
    let dest = ObjectStoreDestination::connect(&uri)
        .await
        .expect("S3 destination connects");

    let logs = tempfile::tempdir().expect("tempdir");
    let state = tempfile::tempdir().expect("tempdir");
    write(&logs.path().join("smoke.log"), TRACING_FIXTURE);

    let t = DrainTarget {
        owner: "log-drain-smoke".to_string(),
        project: format!("run-{}", chrono::Utc::now().timestamp()),
    };
    let cfg = DrainConfig::new(state.path());

    let report = run_once(&cfg, &dest, &t, &[source(logs.path(), Some(Level::Info))])
        .await
        .expect("run_once against S3");
    assert_eq!(report.uploaded, 1);

    let text = read_gunzipped(&dest, &t.object_key("trusty-mpm/smoke.log")).await;
    assert!(text.contains("daemon started"));
    assert!(!text.contains("dropped debug detail"));
}
