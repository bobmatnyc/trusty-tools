//! Minting a brand-new engagement for the auditor to hand out (#5478).
//!
//! Why: everything downstream of an engagement already existed — `distribute`
//! packages one, `package` signs a return with one's key, `verify` checks it —
//! and nothing created one. The auditor hand-edited a template: paste the
//! OpenRouter key, decide the window in prose, and, for a signed engagement,
//! produce an ed25519 seed with some other program and paste that too. Every
//! one of those steps is silently skippable, and the run that skips the signing
//! key exits 0 with an unsigned package. This is that hand-editing as one verb.
//!
//! What: [`create`] writes one directory holding the engagement config and the
//! retained public key. It reuses [`crate::config::generate_for_new_engagement`]
//! for the credential and [`crate::config::with_engagement_identity`] for the
//! window, the minted key and the labels, so the crate still has exactly one
//! writer of a plaintext credential into a config file.
//!
//! ## Nothing here opens a socket
//!
//! Owner's requirement (#5478): no provisioning API, no key minting service.
//! The OpenRouter key is the operator's, conveyed out of band, and the signing
//! keypair comes from the operating system's randomness. The tool pins come
//! from the TEMPLATE rather than from a release lookup, which is the one place
//! this could have reached the network — `crate::cli::bootstrap` resolves
//! "latest published" by asking crates.io, and this capability deliberately
//! does not.
//! Test: `engagement_tests::minting_an_engagement_reaches_no_network`.
//!
//! ## Which half of the keypair goes where
//!
//! #5481's layout, unchanged. The PRIVATE seed is written into the config, at
//! mode 0600, because it travels to the recipient inside the inbound package
//! and they sign locally with no call back to the auditor. The PUBLIC half is
//! written beside it, in a file that never travels — a key that arrives with
//! the thing it authenticates authenticates nothing — and is what
//! `trusty-audit verify --public-key` reads months later.
//!
//! They are two files and one fact, so they are published together, through
//! [`crate::workdir::stage_pair`]: a config carrying seed B beside the public
//! half of seed A parses, loads, and reports nothing until a genuine package is
//! rejected weeks later. A `--force` remint that fails partway is the case that
//! forced this — before #5478's fix it replaced the config first and deleted it
//! on the public half's failure, leaving neither engagement.
//! Test: `engagement_tests::a_forced_remint_that_cannot_write_the_public_half_leaves_the_previous_engagement_whole`.
//!
//! 🔴 [`crate::distribute::assemble`] drops `[signing]` from any config it is
//! handed, because for THAT capability the input is a reusable template and the
//! key in it belongs to whichever engagement it was minted for (#5861). So a
//! package built by pointing `distribute` at this file ships unsigned.
//!
//! Test: `engagement_tests`.

use std::path::{Path, PathBuf};

use trusty_common::file_lock::{lock_path, with_exclusive_lock};

use crate::config::{self, EngagementConfig, EngagementIdentity, SecretKey};
use crate::error::AuditError;
use crate::package::signing::EngagementKey;
use crate::workdir;

/// The file the auditor keeps: the retained public half, 64 hex characters.
///
/// The name `trusty-audit verify --public-key` already documents in the README,
/// so an operator who followed one page finds the file the other page names.
pub const PUBLIC_KEY_FILE_NAME: &str = "retained.pub";

/// What the operator chose about the engagement to mint.
///
/// Why: carried as data rather than read from argv or the environment inside
/// [`create`], for the reason [`crate::distribute::DistributeOptions`] is —
/// a Tauri shell offers the same choices through a form (#5502).
/// What: where it lands, how far back it looks, who it is for, and whether an
/// engagement already in that directory may be replaced.
/// Test: `engagement_tests::a_minted_engagement_loads_back_with_every_field`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NewEngagementOptions {
    /// Directory to write the engagement into. Created when it is absent.
    pub directory: PathBuf,
    /// ISO weeks of history the engagement assesses. Never zero.
    pub window_weeks: u32,
    /// The client this engagement is for, when the operator named one.
    pub client: Option<String>,
    /// The engagement's own label, when the operator named one.
    pub engagement: Option<String>,
    /// Replace an engagement already in `directory`.
    pub force: bool,
}

impl Default for NewEngagementOptions {
    fn default() -> Self {
        Self {
            directory: PathBuf::new(),
            window_weeks: config::DEFAULT_AUDIT_WINDOW_WEEKS,
            client: None,
            engagement: None,
            force: false,
        }
    }
}

/// The engagement that was minted: what to hand out, and what to keep.
///
/// Why: the operator has to be told which of the two files never leaves their
/// machine, at the moment both are written — the mistake this capability makes
/// possible is sending the directory rather than the config.
/// What: both paths, the fingerprint the return package will carry, the window
/// that was written, and the template fields that did not survive.
/// Test: `engagement_tests::a_minted_engagement_loads_back_with_every_field`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewEngagement {
    /// The directory that was written.
    pub directory: PathBuf,
    /// The engagement config, mode 0600. This is what `distribute` packages.
    pub config: PathBuf,
    /// The retained public key. Keep this; it never travels.
    pub retained_public_key: PathBuf,
    /// The signing key's short fingerprint, as the return package records it.
    pub fingerprint: String,
    /// ISO weeks of history the engagement declares.
    pub window_weeks: u32,
    /// Template fields that belonged to another engagement and did not travel.
    pub dropped: Vec<String>,
    /// The template this engagement's pins and instructions came from.
    pub template: PathBuf,
}

/// Mint an engagement from `template_path` into `options.directory`.
///
/// # Preconditions
///
/// `template_path` is a loadable [`EngagementConfig`] — in practice the
/// auditor's own `engagement-template.toml`, which is where the tool pins and
/// the instructions come from. `key` is the OpenRouter key the operator
/// supplied; a blank one is written through as blank, which is what
/// `crate::cli::credential` turns into the recipient's first-run prompt.
///
/// # Postconditions
///
/// On `Ok`, `directory` holds an `engagement.toml` at mode 0600 that parses as
/// an [`EngagementConfig`], carrying `key`, the declared window and a signing
/// seed no other engagement has, plus [`PUBLIC_KEY_FILE_NAME`] holding that
/// seed's public half. On `Err`, nothing at either path has changed: a
/// `force` that fails leaves the previous engagement whole, and a mint into an
/// empty directory leaves it empty. The two halves are never observed
/// disagreeing — a config on disk is always accompanied by the public half of
/// the seed it carries, whichever engagement that is (#5478).
///
/// What: parse the template, mint the keypair, run the two substitutions in
/// order — [`crate::config::generate_for_new_engagement`] to strip what
/// belonged to the previous engagement and write the credential, then
/// [`crate::config::with_engagement_identity`] to write this one's — and
/// publish both files as one unit through [`crate::workdir::stage_pair`],
/// under the engagement config's exclusive lock. The config is staged 0600, so
/// it is owner-only from the moment it exists and a symlink pre-planted at the
/// temporary's name is refused rather than followed (#5868).
///
/// This blocks on that lock, which `crate::registry` takes over the same file.
/// It is not reentrant: a caller already holding it self-deadlocks.
/// Test: `engagement_tests`.
///
/// # Errors
///
/// [`AuditError::MissingPackageInput`] when `template_path` is not a file,
/// [`AuditError::EngagementExists`] when the directory already holds one and
/// `force` was not passed, [`AuditError::Read`] when the template cannot be
/// read, [`AuditError::Parse`] when the template is not a config or the
/// substituted result would not load, [`AuditError::Render`] when the
/// substituted table cannot be serialized, and
/// [`AuditError::EngagementNotCreated`] when the lock cannot be taken or either
/// file cannot be written.
pub fn create(
    template_path: &Path,
    options: &NewEngagementOptions,
    key: &SecretKey,
) -> Result<NewEngagement, AuditError> {
    if !template_path.is_file() {
        return Err(AuditError::MissingPackageInput {
            what: "engagement config template",
            path: template_path.to_path_buf(),
        });
    }

    let config_path = EngagementConfig::default_path(&options.directory);
    let public_key_path = options.directory.join(PUBLIC_KEY_FILE_NAME);
    // #5478: refuse before anything is minted, so an operator who pointed the
    // verb at last month's engagement learns it here rather than from a
    // replaced signing key they can no longer reproduce. This probe is the
    // early, cheap half of the refusal; the one that decides is the same check
    // under the lock in `publish`, which is where a racing mint is caught.
    if !options.force {
        refuse_if_occupied(&config_path, &public_key_path)?;
    }

    let template_text =
        std::fs::read_to_string(template_path).map_err(|source| AuditError::Read {
            path: template_path.to_path_buf(),
            source,
        })?;

    // #5478: the OS's randomness, never a service. `EngagementKey::generate`
    // is the crate's one minting site and this is its production caller.
    let signing_key = EngagementKey::generate();

    // Order matters. The first call DROPS `[signing]` — for it, the input is a
    // template whose key belongs to some other engagement (#5861) — so the
    // freshly minted one has to be written by the second. Both run BEFORE
    // either file is touched, so a template that cannot produce a loadable
    // config leaves the target directory as it found it.
    let (stripped, mut dropped) =
        config::generate_for_new_engagement(&template_text, key, template_path)?;
    let (rendered, dropped_labels) = config::with_engagement_identity(
        &stripped,
        &EngagementIdentity {
            window_weeks: options.window_weeks,
            signing_key: &signing_key.private_hex(),
            client: options.client.as_deref(),
            engagement: options.engagement.as_deref(),
        },
        template_path,
    )?;
    dropped.extend(dropped_labels);

    // A trailing newline because the public half is read by `verify
    // --public-key`, which trims, and by an operator's `cat`, which does not.
    let public_text = format!("{}\n", signing_key.public_hex());
    // #5478: the check and the writes run under ONE lock, so a second `create`
    // aimed at this directory cannot pass the check while this one is between
    // its own check and its own commit. `crate::registry` locks the same file
    // through the same sidecar, so a mint also serialises against a target
    // being registered. A lock that cannot be taken is a refusal, never an
    // unserialised write.
    let published = with_exclusive_lock(&config_path, || {
        publish(&config_path, &rendered, &public_key_path, &public_text, options.force)
    })
    .map_err(|source| AuditError::EngagementNotCreated {
        path: config_path.clone(),
        source: Box::new(AuditError::WorkDir {
            path: lock_path(&config_path),
            source,
        }),
    })?;
    published?;

    Ok(NewEngagement {
        directory: options.directory.clone(),
        config: config_path,
        retained_public_key: public_key_path,
        fingerprint: signing_key.fingerprint(),
        window_weeks: options.window_weeks,
        dropped,
        template: template_path.to_path_buf(),
    })
}

/// Decide the directory is free, then publish both halves as one unit.
///
/// Why (#5478): the two halves of one keypair are two files, and every way of
/// writing them one at a time has a moment where the pair on disk disagrees. A
/// forced remint made that moment destructive: the config was replaced first,
/// so a failure on the public half deleted the config that had just replaced
/// the previous engagement's and left the directory with neither half of
/// either. What is on disk here is either the previous engagement, whole, or
/// the new one, whole.
/// What: the authoritative existence check, then
/// [`crate::workdir::stage_pair`] — which does every fallible write before
/// anything is published — and its commit. The caller holds the engagement
/// config's exclusive lock around both, so the check and the commit cannot be
/// separated by another mint.
/// Test: `engagement_tests::a_forced_remint_that_cannot_write_the_public_half_leaves_the_previous_engagement_whole`,
/// `engagement_tests::a_forced_remint_leaves_a_matching_pair`.
///
/// # Errors
///
/// [`AuditError::EngagementExists`] when either half is already there and
/// `force` was not passed, [`AuditError::EngagementNotCreated`] wrapping the
/// [`AuditError::WorkDir`] that names the file which could not be written.
fn publish(
    config_path: &Path,
    rendered: &str,
    public_key_path: &Path,
    public_text: &str,
    force: bool,
) -> Result<(), AuditError> {
    if !force {
        refuse_if_occupied(config_path, public_key_path)?;
    }
    workdir::stage_pair((config_path, rendered), (public_key_path, public_text))
        .and_then(workdir::StagedPair::commit)
        .map_err(|source| AuditError::EngagementNotCreated {
            path: config_path.to_path_buf(),
            source: Box::new(source),
        })
}

/// [`AuditError::EngagementExists`] when either half is already in place.
///
/// A lone `retained.pub` counts: minting over it orphans the half that is
/// there. Test: `engagement_tests::a_lone_retained_public_key_also_blocks_a_mint`.
fn refuse_if_occupied(config_path: &Path, public_key_path: &Path) -> Result<(), AuditError> {
    for existing in [config_path, public_key_path] {
        if existing.exists() {
            return Err(AuditError::EngagementExists {
                path: existing.to_path_buf(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod engagement_tests {
    use super::*;
    use crate::package::signing::RetainedKey;

    const TEMPLATE: &str = r#"
openrouter_key = ""
instructions = "Assess the last 52 weeks."
client = "Previous Client"
engagement = "Previous Engagement"

[tools]
tga = "2.9.4"
trusty-search = "0.47.0"
trusty-analyze = "0.9.2"
trusty-review = "0.15.1"

[boards.jira]
url = "https://previous.atlassian.net"
email = "auditor@previous.example"
token = "previous-jira-token"

[signing]
private_key = "0000000000000000000000000000000000000000000000000000000000000000"

[report]
investigate_max_files = 240
"#;

    /// A dummy that looks like a key and is not one. Never a live credential:
    /// a credential scan over this source must not have to decide.
    const KEY: &str = "sk-or-v1-not-a-real-key";

    fn template_at(dir: &Path) -> PathBuf {
        let path = dir.join("engagement-template.toml");
        std::fs::write(&path, TEMPLATE).expect("template written");
        path
    }

    fn options(dir: &Path) -> NewEngagementOptions {
        NewEngagementOptions {
            directory: dir.join("acme-2026"),
            client: Some("Acme".to_owned()),
            engagement: Some("2026 due diligence".to_owned()),
            ..NewEngagementOptions::default()
        }
    }

    /// Acceptance (a): every file #5478 names exists, and the config it wrote
    /// loads back carrying each top-level key the engagement declares.
    #[test]
    fn a_minted_engagement_loads_back_with_every_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let minted = create(&template, &options(tmp.path()), &SecretKey::new(KEY))
            .expect("the engagement is minted");

        assert!(minted.config.is_file(), "engagement.toml is written");
        assert!(
            minted.retained_public_key.is_file(),
            "the retained public key is written"
        );
        assert_eq!(
            minted.config.file_name().expect("named"),
            EngagementConfig::FILE_NAME
        );
        assert_eq!(
            minted.retained_public_key.file_name().expect("named"),
            PUBLIC_KEY_FILE_NAME
        );

        let loaded = EngagementConfig::load(&minted.config).expect("the minted config loads");
        assert_eq!(loaded.openrouter_key.expose(), KEY);
        assert_eq!(loaded.audit.window_weeks, 52);
        assert_eq!(loaded.client.as_deref(), Some("Acme"));
        assert_eq!(loaded.engagement.as_deref(), Some("2026 due diligence"));
        assert_eq!(loaded.tools.tga.version(), "2.9.4");
        assert_eq!(loaded.report.investigate_max_files, Some(240));
        assert!(loaded.signing.private_key.is_some(), "a seed was minted");
    }

    /// The private half is in the config and the public half is beside it, and
    /// they are halves of ONE keypair — the property `verify` depends on months
    /// later, when the config is long gone.
    #[test]
    fn the_retained_public_key_is_the_minted_private_keys_own_half() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let minted = create(&template, &options(tmp.path()), &SecretKey::new(KEY))
            .expect("the engagement is minted");

        let loaded = EngagementConfig::load(&minted.config).expect("loads");
        let private = loaded
            .signing_key()
            .expect("the seed parses")
            .expect("a seed was written");
        let retained_hex = std::fs::read_to_string(&minted.retained_public_key).expect("readable");
        let retained = RetainedKey::from_hex(retained_hex.trim()).expect("the public half parses");

        assert_eq!(private.public_hex(), retained_hex.trim());
        assert_eq!(private.fingerprint(), minted.fingerprint);
        assert_eq!(private.fingerprint(), retained.fingerprint());
    }

    /// Two engagements minted from one template must not share a key: a client
    /// holding one could otherwise sign as the other.
    #[test]
    fn two_engagements_from_one_template_get_different_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let first = create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("first");
        let mut second_options = options(tmp.path());
        second_options.directory = tmp.path().join("beta-2026");
        let second = create(&template, &second_options, &SecretKey::new(KEY)).expect("second");

        assert_ne!(first.fingerprint, second.fingerprint);
    }

    /// #5861, one field over: the template's client name is Acme's, and it must
    /// not reach the file Beta reads.
    #[test]
    fn the_templates_previous_client_never_reaches_the_minted_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let minted = create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("minted");

        let text = std::fs::read_to_string(&minted.config).expect("readable");
        assert!(!text.contains("Previous Client"), "{text}");
        assert!(!text.contains("previous-jira-token"), "{text}");
        assert!(
            minted.dropped.iter().any(|d| d == "boards.jira"),
            "{:?}",
            minted.dropped
        );
    }

    /// The template's `[signing]` seed belongs to another engagement, and the
    /// minted one must be the freshly generated key rather than that one.
    #[test]
    fn the_templates_signing_seed_is_replaced_not_carried() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let minted = create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("minted");

        let text = std::fs::read_to_string(&minted.config).expect("readable");
        assert!(
            !text.contains("0000000000000000000000000000000000000000000000000000000000000000"),
            "{text}"
        );
    }

    /// An operator who names no labels gets none, rather than the template's.
    #[test]
    fn unnamed_labels_are_dropped_and_reported() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let minted = create(
            &template,
            &NewEngagementOptions {
                directory: tmp.path().join("unlabelled"),
                ..NewEngagementOptions::default()
            },
            &SecretKey::new(KEY),
        )
        .expect("minted");

        let loaded = EngagementConfig::load(&minted.config).expect("loads");
        assert!(loaded.client.is_none());
        assert!(loaded.engagement.is_none());
        assert!(minted.dropped.iter().any(|d| d == "client"));
        assert!(minted.dropped.iter().any(|d| d == "engagement"));
    }

    #[test]
    fn a_declared_window_reaches_the_minted_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let minted = create(
            &template,
            &NewEngagementOptions {
                directory: tmp.path().join("half-year"),
                window_weeks: 26,
                ..NewEngagementOptions::default()
            },
            &SecretKey::new(KEY),
        )
        .expect("minted");

        assert_eq!(minted.window_weeks, 26);
        let loaded = EngagementConfig::load(&minted.config).expect("loads");
        assert_eq!(loaded.audit.window_weeks, 26);
    }

    /// The config carries a credential, so its mode is part of the contract.
    #[cfg(unix)]
    #[test]
    fn the_minted_config_is_owner_only_and_the_public_key_is_not() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let minted = create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("minted");

        let mode = std::fs::metadata(&minted.config)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the config carries the credential");
        assert!(minted.retained_public_key.is_file());
    }

    /// Acceptance (c): a second mint into the same directory REFUSES rather
    /// than replacing, and leaves the first engagement byte-identical.
    #[test]
    fn an_existing_engagement_is_never_replaced_without_force() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let first = create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("first");
        let before = std::fs::read_to_string(&first.config).expect("readable");
        let public_before = std::fs::read_to_string(&first.retained_public_key).expect("readable");

        let err = create(&template, &options(tmp.path()), &SecretKey::new(KEY))
            .expect_err("the second call refuses");
        assert!(
            matches!(err, AuditError::EngagementExists { .. }),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&first.config).expect("readable"),
            before,
            "the refused call must not have rewritten anything"
        );
        assert_eq!(
            std::fs::read_to_string(&first.retained_public_key).expect("readable"),
            public_before,
            "the retained public half must survive a refused mint"
        );
    }

    /// A retained public key with no config beside it is still an engagement in
    /// that directory: minting over it would orphan the half that is there.
    #[test]
    fn a_lone_retained_public_key_also_blocks_a_mint() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let opts = options(tmp.path());
        std::fs::create_dir_all(&opts.directory).expect("directory");
        std::fs::write(opts.directory.join(PUBLIC_KEY_FILE_NAME), "abc\n").expect("planted");

        let err =
            create(&template, &opts, &SecretKey::new(KEY)).expect_err("the lone half blocks it");
        assert!(
            matches!(err, AuditError::EngagementExists { .. }),
            "{err:?}"
        );
    }

    /// Read the pair back and assert the config's seed and the retained file
    /// are halves of ONE key — the property `verify --public-key` rests on.
    fn assert_pair_agrees(config: &Path, public: &Path) {
        let loaded = EngagementConfig::load(config).expect("the config on disk loads");
        let private = loaded
            .signing_key()
            .expect("the seed parses")
            .expect("a seed is present");
        let retained = std::fs::read_to_string(public).expect("the retained half is readable");
        assert_eq!(
            private.public_hex(),
            retained.trim(),
            "the config's seed and {} must be halves of one key",
            public.display()
        );
    }

    /// #5478, the CRITICAL half: a `--force` remint whose PUBLIC half cannot be
    /// written must leave the engagement that is already there whole.
    ///
    /// Before the staged-pair publish, `create` replaced the config first, so
    /// this sequence overwrote engagement A's config with B's and then deleted
    /// it when the public write failed — leaving no config at all beside A's
    /// `retained.pub`, and losing A's seed with it. Now every fallible write
    /// happens before anything is published, so the failure cannot reach A.
    ///
    /// The failure is injected by occupying the public half's staging path with
    /// a DIRECTORY: the exclusive create cannot make a file there and the
    /// one unlink recovery cannot remove a directory. A pre-planted temporary
    /// is the same seam `crate::registry`'s tests use on the lock sidecar.
    #[test]
    fn a_forced_remint_that_cannot_write_the_public_half_leaves_the_previous_engagement_whole() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let first = create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("first");
        let config_before = std::fs::read_to_string(&first.config).expect("readable");
        let public_before = std::fs::read_to_string(&first.retained_public_key).expect("readable");

        std::fs::create_dir_all(workdir::staging_path(&first.retained_public_key))
            .expect("the public half's staging path is occupied");
        let err = create(
            &template,
            &NewEngagementOptions {
                force: true,
                ..options(tmp.path())
            },
            &SecretKey::new(KEY),
        )
        .expect_err("the public half cannot be written, so the remint fails");

        assert!(
            matches!(err, AuditError::EngagementNotCreated { .. }),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&first.config).expect("the previous config is still there"),
            config_before,
            "a failed remint must not have touched the previous engagement's config"
        );
        assert_eq!(
            std::fs::read_to_string(&first.retained_public_key).expect("still there"),
            public_before,
            "a failed remint must not have touched the previous retained key"
        );
        assert_pair_agrees(&first.config, &first.retained_public_key);
        assert!(
            !workdir::staging_path(&first.config).exists(),
            "the config's temporary must not survive the failure"
        );
    }

    /// The successful half of the same guarantee: after a `--force` remint the
    /// pair on disk is the NEW pair, and it agrees.
    #[test]
    fn a_forced_remint_leaves_a_matching_pair() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let first = create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("first");
        let second = create(
            &template,
            &NewEngagementOptions {
                force: true,
                ..options(tmp.path())
            },
            &SecretKey::new(KEY),
        )
        .expect("force replaces it");

        assert_pair_agrees(&second.config, &second.retained_public_key);
        let retained =
            std::fs::read_to_string(&second.retained_public_key).expect("the retained half");
        assert_ne!(
            first.fingerprint, second.fingerprint,
            "a remint mints a new key"
        );
        assert_eq!(
            RetainedKey::from_hex(retained.trim())
                .expect("parses")
                .fingerprint(),
            second.fingerprint,
            "the retained half is the NEW key's, not the replaced one's"
        );
        assert!(
            !workdir::staging_path(&second.config).exists()
                && !workdir::staging_path(&second.retained_public_key).exists(),
            "a successful remint leaves no temporary behind"
        );
    }

    #[test]
    fn force_replaces_the_engagement_and_mints_a_new_key() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let first = create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("first");
        let second = create(
            &template,
            &NewEngagementOptions {
                force: true,
                ..options(tmp.path())
            },
            &SecretKey::new(KEY),
        )
        .expect("force replaces it");

        assert_ne!(first.fingerprint, second.fingerprint);
    }

    #[test]
    fn a_template_that_is_not_there_is_named_rather_than_parsed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let err = create(
            &tmp.path().join("absent.toml"),
            &options(tmp.path()),
            &SecretKey::new(KEY),
        )
        .expect_err("no template");
        assert!(
            matches!(err, AuditError::MissingPackageInput { .. }),
            "{err:?}"
        );
    }

    /// Acceptance (b): a template missing a REQUIRED field is a typed refusal.
    /// The three tool pins are required, so a template naming two of them
    /// cannot silently resolve the third — that is #5454's version skew, which
    /// the config's own schema closed and this verb must not reopen. Nothing is
    /// written, so a failed mint is not an engagement.
    #[test]
    fn a_template_missing_a_required_pin_is_a_typed_error_and_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = tmp.path().join("partial.toml");
        std::fs::write(
            &template,
            "openrouter_key = \"\"\ninstructions = \"x\"\n\n\
             [tools]\ntga = \"2.9.4\"\ntrusty-search = \"0.47.0\"\n",
        )
        .expect("written");
        let opts = options(tmp.path());

        let err = create(&template, &opts, &SecretKey::new(KEY))
            .expect_err("an incomplete template cannot mint an engagement");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
        assert!(
            !EngagementConfig::default_path(&opts.directory).exists(),
            "a refused mint must leave no config behind"
        );
        assert!(
            !opts.directory.join(PUBLIC_KEY_FILE_NAME).exists(),
            "a refused mint must leave no retained key behind"
        );
    }

    /// Acceptance (b), the Fail-Open Check: a zero-week window would produce an
    /// engagement that assesses no history and still exits 0. It is refused,
    /// and refused before anything is written.
    #[test]
    fn a_zero_week_window_is_refused_and_writes_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let opts = NewEngagementOptions {
            window_weeks: 0,
            ..options(tmp.path())
        };

        let err = create(&template, &opts, &SecretKey::new(KEY))
            .expect_err("a zero-week engagement is not one");
        assert!(matches!(err, AuditError::Parse { .. }), "{err:?}");
        assert!(!EngagementConfig::default_path(&opts.directory).exists());
    }

    /// The write half of the Fail-Open Check: the retained public key cannot be
    /// written, so the mint FAILS rather than reporting an engagement whose
    /// signatures nobody could ever check — and it takes the orphaned config
    /// with it. A directory at the target path is a write that cannot succeed.
    #[test]
    fn a_config_written_without_its_retained_key_is_not_left_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let opts = NewEngagementOptions {
            force: true,
            ..options(tmp.path())
        };
        std::fs::create_dir_all(opts.directory.join(PUBLIC_KEY_FILE_NAME))
            .expect("the target path is occupied by a directory");

        let err = create(&template, &opts, &SecretKey::new(KEY))
            .expect_err("the retained key cannot be written");
        assert!(
            matches!(err, AuditError::EngagementNotCreated { .. }),
            "{err:?}"
        );
        assert!(
            !EngagementConfig::default_path(&opts.directory).exists(),
            "the orphaned config must not survive the failure"
        );
    }

    /// A blank key is written through as blank, which is what the recipient's
    /// first run reads as "ask me" — the same contract `distribute
    /// --prompt-for-key` relies on (#5483).
    #[test]
    fn a_blank_key_is_written_through_rather_than_refused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        let minted = create(&template, &options(tmp.path()), &SecretKey::new(""))
            .expect("a blank key is a supported state");

        let loaded = EngagementConfig::load(&minted.config).expect("loads");
        assert!(loaded.openrouter_key.is_empty());
    }

    /// The owner's no-network requirement (#5478), as a gate rather than a
    /// claim. Two independent checks, because either alone is weak:
    ///
    /// - [`create`] is a plain `fn`. Every network leg in this crate is `async`
    ///   and needs a reactor, so a synchronous call cannot await one.
    /// - No HTTP client type is named anywhere in this module's PRODUCTION
    ///   half, so a future key-provisioning call fails here instead of
    ///   shipping. The source is split at the test module rather than scanned
    ///   whole, so this test's own needles do not match themselves.
    #[test]
    fn minting_an_engagement_reaches_no_network() {
        let _synchronous: fn(
            &Path,
            &NewEngagementOptions,
            &SecretKey,
        ) -> Result<NewEngagement, AuditError> = create;

        const SOURCE: &str = include_str!("engagement.rs");
        let production = SOURCE
            .split("#[cfg(test)]")
            .next()
            .expect("split always yields a first part");
        for client in ["reqwest", "octocrab", "TcpStream", "PinResolver"] {
            assert!(
                !production.contains(client),
                "minting an engagement must not reach the network, and this module names {client}"
            );
        }

        let tmp = tempfile::tempdir().expect("tempdir");
        let template = template_at(tmp.path());
        create(&template, &options(tmp.path()), &SecretKey::new(KEY)).expect("minted");
    }
}
