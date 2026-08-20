//! The working directory `trusty-audit` owns.
//!
//! Why: the crate downloads pinned tools, clones the recipient's repositories,
//! builds a tga extract database, and records run progress. Every one of those
//! is a file it writes on someone else's machine, and #5502 designs the layout
//! now rather than retrofitting one once four features have each picked their
//! own path. Two properties follow from designing it here:
//!
//! - **Everything this module computes is under one root.** No path in the
//!   layout escapes it — `layout_tests::every_layout_path_is_inside_the_root`
//!   proves that, and the README's deletion instructions rest on it.
//!
//!   `rm -rf <root>` is no longer a COMPLETE uninstall, and #5915 is where that
//!   stopped being true: approving a checkout for indexing writes a row to
//!   `trusty-search`'s own allowlist, outside this root and outside this crate's
//!   reach. `trusty-search index remove <checkout>` is what undoes it. The
//!   README says so rather than promising a completeness that is not there.
//! - **The root holds the recipient's source.** `repos/` is their checkouts and
//!   `extract/` is derived from them, so this is a data-handling surface. The
//!   README states what is written where (#5494).
//!
//! What: [`WorkDir`], a pure path calculator — it computes the layout without
//! touching disk, and [`WorkDir::create`] is the one method on it that does I/O.
//! [`WorkDir::resolve`] takes the environment as an argument rather than reading
//! it, so resolution order is testable without mutating process state.
//!
//! [`write_atomically`] is the module's one free function, and the one writer
//! for every state file under the root: `state/selected-repos.toml`
//! (`crate::run`) and `state/audit-targets.toml` (`crate::registry`) both go
//! through it, so the temp-file-then-rename discipline is decided once (#5822).
//! [`write_private_atomically`] is the same writer at mode 0600, for the one
//! file that carries a credential (#5868).
//! Test: `super::layout_tests`.
//!
//! ## Open questions, recorded rather than resolved (#5502)
//!
//! 1. ~~**Where the root should live.**~~ Settled by #5915:
//!    `~/.trusty-tools/trusty-audit/work`. The scaffold put it beside the
//!    unzipped folder so the recipient could see it, but `trusty-search` refuses
//!    to index a checkout under `/tmp`, `/var/folders`, `~/Downloads`,
//!    `~/Desktop` or `~/Documents` — and those are where an emailed package gets
//!    unzipped — so the placement silently cost every run its code-analysis leg.
//!    See [`WorkDir::resolve`]. The property below is what this trades away.
//! 2. **Whether two runs may share one root.** Nothing here locks, and the
//!    scaffold does not decide. Sharing is attractive (a second engagement
//!    reuses the downloaded tools) and dangerous (`state/` and `out/` are
//!    per-run). A lock file or a per-engagement subdirectory both answer it.
//! 3. **Deletion semantics.** Deleting the root mid-run loses run progress and
//!    the clones; nothing outside the root is affected. Whether a `clean` verb
//!    should exist — and whether it should refuse to run while a run is in
//!    flight — is open.

use std::path::{Path, PathBuf};

use crate::error::AuditError;

/// Environment variable that overrides the default working-directory root.
pub const WORKDIR_ENV: &str = "TRUSTY_AUDIT_WORKDIR";

/// Directory name appended to the current directory when home cannot be found.
pub const DEFAULT_WORKDIR_NAME: &str = "trusty-audit-work";

/// This crate's directory under `~/.trusty-tools`, per the workspace convention
/// `trusty_common::crate_config` owns.
const CRATE_NAME: &str = "trusty-audit";

/// Subdirectory of `~/.trusty-tools/trusty-audit` the work root occupies.
///
/// `config.yaml` is the sibling `crate_config` writes, so the run's own tree is
/// kept one level down rather than sharing that directory.
const WORK_SUBDIR: &str = "work";

/// One entry in the working-directory layout.
///
/// Why: the CLI prints the layout, the README documents it, and the containment
/// test walks it — all three want the same list, so the list is data.
/// What: a stable identifier plus one line of what lands there.
/// Test: `super::layout_tests::every_layout_path_is_inside_the_root`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Area {
    /// Pinned tool binaries, installed via `trusty-installer` (#5491).
    Tools,
    /// Clones of the recipient's repositories — their source code.
    Repos,
    /// The tga extract database, derived from those clones.
    Extract,
    /// Run state: repo selection and per-repo progress (#5494).
    State,
    /// The deliverable that goes back: report plus manifest. Never a credential.
    Output,
    /// Logs from the tools this crate runs.
    Logs,
}

impl Area {
    /// Every area, in the order the CLI and README present them.
    pub const ALL: [Area; 6] = [
        Area::Tools,
        Area::Repos,
        Area::Extract,
        Area::State,
        Area::Output,
        Area::Logs,
    ];

    /// Directory name under the root.
    pub fn dir_name(self) -> &'static str {
        match self {
            Area::Tools => "tools",
            Area::Repos => "repos",
            Area::Extract => "extract",
            Area::State => "state",
            Area::Output => "out",
            Area::Logs => "logs",
        }
    }

    /// One line describing what is written there, for the CLI and the README.
    pub fn description(self) -> &'static str {
        match self {
            Area::Tools => "pinned tga / trusty-analyze / trusty-review binaries",
            Area::Repos => "clones of your repositories — your source code",
            Area::Extract => "the tga extract database built from those clones",
            Area::State => "repo selection and run progress",
            Area::Output => {
                "the deliverable to return — report and manifest, never a key; \
                 start at index.md"
            }
            Area::Logs => "output from the tools this client runs",
        }
    }
}

/// The directory tree `trusty-audit` owns on the recipient's machine.
///
/// Why: see the module docs — one root so deletion is complete, and a pure path
/// calculator so the layout can be asserted without a filesystem.
/// What: wraps an absolute-or-relative root; every accessor is `root.join(...)`.
/// Test: `super::layout_tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkDir {
    root: PathBuf,
}

impl WorkDir {
    /// Wrap an explicit root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Decide the root from the flag, then the environment, then the default,
    /// and anchor it to `cwd` when it is relative.
    ///
    /// Why: the caller (`main.rs`) reads the process environment once and passes
    /// it in, which keeps this function pure — resolution order is provable
    /// without `std::env::set_var` and its cross-test races.
    ///
    /// Anchoring is what makes the root usable by a CHILD process (#5672).
    /// `run::sweep` spawns `tga audit` with the root as the child's working
    /// directory, and a relative program path is resolved against the CHILD's
    /// cwd, not the parent's — so `--work-dir w` named `w/tools/tga` to a child
    /// already sitting in `w`, and the spawn failed with `os error 2`. Every
    /// path this crate hands a child descends from the root, so anchoring the
    /// root once here fixes the tool binaries, the generated tga config, the
    /// output directory and the extract database together.
    ///
    /// What: `explicit` wins; else `env_value` (the value of [`WORKDIR_ENV`]);
    /// else `~/.trusty-tools/trusty-audit/work`; else — only when `home` is
    /// `None` — [`DEFAULT_WORKDIR_NAME`] beside the cwd. Then `cwd.join(...)`,
    /// which returns an absolute choice unchanged. An empty env value is
    /// ignored, since `TRUSTY_AUDIT_WORKDIR=` is far more likely to be an unset
    /// shell variable than a request to use the filesystem root.
    ///
    /// #5915: the default moved out of the cwd because the cwd is wherever the
    /// recipient unzipped the package, and `trusty-search` refuses to index a
    /// checkout under one — `/var/folders` and `/tmp` by
    /// `SENSITIVE_PATH_PREFIXES`, and `~/Downloads`, `~/Desktop`, `~/Documents`
    /// by `SENSITIVE_HOME_TOP_DIRS`. `~/Downloads` is where a recipient
    /// unzipping an emailed package lands by default, so the refusal was the
    /// ordinary case rather than an edge one, and it cost the whole
    /// code-analysis leg. `~/.trusty-tools/<crate>` is the one home location the
    /// denylist permits, and it is already this workspace's convention —
    /// [`trusty_common::crate_config`] owns the path so this crate does not spell
    /// a second one.
    ///
    /// `home` is passed in rather than read, for the same reason `env_value` is.
    ///
    /// This joins rather than calling `std::fs::canonicalize`, deliberately.
    /// The root normally does not exist yet — [`WorkDir::create`] makes it — and
    /// `canonicalize` fails on a path that is not already on disk, so it cannot
    /// run at this boundary at all. It would also resolve symlinks, which would
    /// print a root the recipient never typed and weaken the README's
    /// `rm -rf <root>` instruction. Joining needs no I/O and cannot fail.
    /// Test: `super::layout_tests::resolution_order_is_flag_then_env_then_default`,
    /// `super::layout_tests::a_relative_choice_is_anchored_to_the_cwd`,
    /// `super::layout_tests::the_default_root_is_one_trusty_search_will_index`.
    pub fn resolve(
        explicit: Option<PathBuf>,
        env_value: Option<&str>,
        home: Option<&Path>,
        cwd: &Path,
    ) -> Self {
        let chosen = explicit
            .or_else(|| {
                env_value
                    .filter(|v| !v.trim().is_empty())
                    .map(PathBuf::from)
            })
            .unwrap_or_else(|| default_root(home));
        Self::new(cwd.join(chosen))
    }

    /// The root of everything this crate writes.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path of one layout area.
    ///
    /// This computes a path; it does not check what is at it. A pre-planted
    /// symlink at an area would send writes outside the root and survive the
    /// delete — `tools::install` refuses that before installing (#5495), and
    /// repo cloning owes the same check when it lands (#5215).
    pub fn path(&self, area: Area) -> PathBuf {
        self.root.join(area.dir_name())
    }

    /// Every area paired with its path, in [`Area::ALL`] order.
    pub fn layout(&self) -> Vec<(Area, PathBuf)> {
        Area::ALL.iter().map(|a| (*a, self.path(*a))).collect()
    }

    /// Create the root and every area directory, idempotently.
    ///
    /// Why: the guided flow's first act is making the working directory exist,
    /// and a partially-created tree is worse than none — a later feature that
    /// assumes `state/` exists would fail somewhere unrelated to the cause.
    /// What: the writability precondition first (see [`Self::writable`]), then
    /// `create_dir_all` per area, which also creates the root. Existing
    /// directories are left alone, so re-running a flow is safe.
    /// Test: `super::layout_tests::create_is_idempotent_and_makes_every_area`,
    /// `super::layout_tests::an_unwritable_cwd_is_named_before_anything_is_created`.
    ///
    /// # Errors
    ///
    /// [`AuditError::WorkDirNotWritable`] when nothing can be written where the
    /// root resolved, and [`AuditError::WorkDir`] naming the first path that
    /// could not be created for any other reason.
    pub fn create(&self) -> Result<(), AuditError> {
        self.writable()?;
        for (_, path) in self.layout() {
            std::fs::create_dir_all(&path).map_err(|source| AuditError::WorkDir {
                path: path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    /// Prove something can be written where this root resolved (#5941).
    ///
    /// Why: `create_dir_all` on `/trusty-audit-work/tools` reports EROFS
    /// against a path that does not exist, so the operator reads it as a
    /// missing directory and goes looking for one. The condition that actually
    /// failed is one level up and one step earlier: the directory creation
    /// would have STARTED in is not writable. It is checked here rather than at
    /// [`WorkDir::resolve`] because resolution is a pure path calculation and
    /// must stay one — and because a root chosen by `--work-dir` or
    /// [`WORKDIR_ENV`] can be just as unwritable as the cwd-relative fallback
    /// [`default_root`] leaves behind when there is no home directory.
    /// What: finds the nearest ancestor of the root that exists — where
    /// `create_dir_all` would begin — and creates and removes a uniquely-named
    /// probe directory in it. A probe rather than a mode check: `/` on macOS is
    /// `drwxr-xr-x root:wheel` on a read-only volume, so its bits say writable
    /// and the write still fails, and the reverse holds for an account whose
    /// group grants access. The probe is removed whether or not the caller goes
    /// on to create anything.
    /// Test: `super::layout_tests::an_unwritable_cwd_is_named_before_anything_is_created`,
    /// `super::layout_tests::an_unwritable_root_leaves_nothing_behind`,
    /// `super::layout_tests::a_writable_root_passes_the_check_and_leaves_no_probe`.
    ///
    /// # Errors
    ///
    /// [`AuditError::WorkDirNotWritable`] naming the root, the directory tried,
    /// and both remedies.
    fn writable(&self) -> Result<(), AuditError> {
        let tried = self
            .root
            .ancestors()
            .find(|path| path.exists())
            .unwrap_or(&self.root);
        let probe = tried.join(format!(".trusty-audit-writable.{}", writer_tag()));
        std::fs::create_dir(&probe).map_err(|source| AuditError::WorkDirNotWritable {
            root: self.root.clone(),
            tried: tried.to_path_buf(),
            env: WORKDIR_ENV,
            source,
        })?;
        let _ = std::fs::remove_dir(&probe);
        Ok(())
    }
}

/// Where the run's tree lands when neither the flag nor the environment names a
/// place (#5915).
///
/// Absolute when `home` is known. The relative fallback is the pre-#5915
/// behaviour, kept only for the case it was never the wrong answer to: a
/// stripped environment with no home directory, where there is nowhere better.
///
/// #5941: this used to add "and the cwd is at least writable", which is false —
/// an operator standing at `/` got a raw `os error 30` naming a directory that
/// was never created. Nothing here can check it, because this is a pure path
/// calculation over a root that normally does not exist yet;
/// [`WorkDir::writable`] asks the question at the moment the answer matters,
/// for every root rather than only this branch's.
fn default_root(home: Option<&Path>) -> PathBuf {
    match home {
        Some(home) => {
            trusty_common::crate_config::crate_config_dir_at(home, CRATE_NAME).join(WORK_SUBDIR)
        }
        None => PathBuf::from(DEFAULT_WORKDIR_NAME),
    }
}

/// The default root as the README and the CLI spell it for a human (#5915).
///
/// The tilde is deliberate: it is what the recipient types to reach the
/// directory, and what a `rm -rf` instruction must name.
pub const DEFAULT_ROOT_DISPLAY: &str = "~/.trusty-tools/trusty-audit/work";

/// Write a state file so no reader can ever observe a partial one.
///
/// Why: `crate::run::SELECTION_FILE` states this obligation on whoever writes
/// it — a producer that crashes mid-write leaves syntactically valid TOML
/// holding a PREFIX of the entries, which reads as a smaller-but-complete
/// document. #5822 adds a second such file (`crate::registry`), so the
/// discipline moved here rather than being restated per producer. #5494 adds a
/// third, and the one that most needs it: the run checkpoint is written after
/// every repository precisely so it survives the crash it exists for, and a
/// torn one would tell the next run a four-hour audit had finished.
/// What: creates the parent directory, writes to a uniquely-named temporary
/// file beside the target, and renames it into place. The rename is atomic; the
/// unique suffix is what lets two writers race without either reading the
/// other's half-written file. A failed rename removes the temporary file.
///
/// The temporary is created with `O_EXCL`, so whatever the suffix names is
/// created here or the write fails — see [`write_temp`], which owns that
/// guarantee and the one recovery it allows.
/// Test: `crate::run::run_tests::racing_writers_never_leave_a_torn_selection`,
/// `crate::registry::registry_tests::a_registry_round_trips_both_kinds`,
/// `super::layout_tests::an_atomic_write_leaves_no_temporary_behind`,
/// `super::layout_tests::a_stale_temporary_is_replaced_not_reused`,
/// `super::layout_tests::an_unpublishable_target_is_an_error_and_leaves_no_temporary`.
///
/// This makes ONE write untearable. It does not make a load-mutate-save
/// indivisible — [`crate::registry::register`] takes a lock for that (#5822).
///
/// # Errors
///
/// [`AuditError::WorkDir`] naming the directory, the temporary file, or the
/// target, depending on which step failed.
pub(crate) fn write_atomically(path: &Path, text: &str) -> Result<(), AuditError> {
    write_with_mode(path, text, None)
}

/// Write a file only its owner can read, without ever tearing it.
///
/// Why: `engagement.toml` carries the OpenRouter credential, and the first-run
/// prompt persists it there (#5868). Under a default 022 umask the plain writer
/// above leaves that file 0644 — readable by every account on the machine,
/// which on a client's shared build host is the whole exposure. The recipient
/// did not choose the mode and should not have to know to check it.
/// What: [`write_atomically`] with the temporary file created at 0600. Two
/// guarantees, and it is worth being exact about which one each mechanism buys:
///
/// - **The bytes go where this function names.** The temporary is opened with
///   `O_EXCL`, so a symlink pre-planted at its (guessable) name is refused
///   rather than followed. Mode 0600 does not provide this on its own: it
///   constrains a file at CREATION, and an open that follows a symlink creates
///   nothing. Before #5868's follow-up fix, that open wrote the plaintext key
///   into whatever the link pointed at.
/// - **The credential is never readable by another account.** 0600 is passed to
///   `open`, so the file holds those bits from the moment it exists, and the
///   rename publishes it under them.
///
/// Neither reaches PAST the rename. A symlink pre-planted at the TARGET is
/// replaced by the rename rather than written through, but this function does
/// not police what the target's parent directory permits (#5495 is that
/// question for the areas it owns).
/// Test: `super::layout_tests::a_private_write_is_owner_only_and_still_atomic`,
/// `super::layout_tests::a_pre_planted_symlink_never_receives_the_credential`.
///
/// # Errors
///
/// As [`write_atomically`].
pub(crate) fn write_private_atomically(path: &Path, text: &str) -> Result<(), AuditError> {
    write_with_mode(path, text, Some(OWNER_ONLY))
}

/// Mode a file holding a credential is created with: owner read/write, nothing else.
#[cfg(unix)]
const OWNER_ONLY: u32 = 0o600;
/// Windows has no mode bits; the constant exists so the signature is uniform.
#[cfg(not(unix))]
const OWNER_ONLY: u32 = 0;

/// The shared temp-file-then-rename writer. See [`write_atomically`].
fn write_with_mode(path: &Path, text: &str, mode: Option<u32>) -> Result<(), AuditError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|source| AuditError::WorkDir {
        path: dir.to_path_buf(),
        source,
    })?;

    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = path.with_file_name(format!("{file_name}.{}.tmp", writer_tag()));
    write_temp(&temp, text, mode).map_err(|source| AuditError::WorkDir {
        path: temp.clone(),
        source,
    })?;
    std::fs::rename(&temp, path).map_err(|source| {
        let _ = std::fs::remove_file(&temp);
        AuditError::WorkDir {
            path: path.to_path_buf(),
            source,
        }
    })
}

/// Create the temporary file holding `text`, at `mode` when one is asked for.
///
/// The open is exclusive ([`create_exclusive`]), which is what decides where
/// the bytes go. Whatever is already at the temporary path — a symlink, a
/// regular file — is refused rather than opened, so the write cannot be
/// redirected through a pre-planted link and cannot land on top of an existing
/// file. A refusal is recovered ONCE by unlinking and re-creating: a temporary
/// left behind by a writer that crashed holding this tag is the ordinary cause,
/// and unlinking a symlink removes the link, never what it points at. The
/// retry is exclusive too, so a racing re-plant loses rather than being
/// followed. There is no second retry — a path that stays occupied is an error
/// the caller must see.
///
/// `mode` is applied twice for two different reasons: [`create_exclusive`]
/// passes it to `open` so the file is never momentarily wider, and
/// [`enforce_mode`] sets it afterwards because `open`'s mode is masked by the
/// process umask, which can only clear bits — an unusual umask would otherwise
/// leave the credential file narrower than asked for, not wider.
fn write_temp(temp: &Path, text: &str, mode: Option<u32>) -> std::io::Result<()> {
    use std::io::Write as _;

    let mut file = match create_exclusive(temp, mode) {
        Err(occupied) if occupied.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_file(temp)?;
            create_exclusive(temp, mode)?
        }
        first => first?,
    };
    if let Some(mode) = mode {
        enforce_mode(&file, mode)?;
    }
    file.write_all(text.as_bytes())
}

/// `open` the temporary with `O_EXCL`, at `mode` when one is asked for.
#[cfg(unix)]
fn create_exclusive(temp: &Path, mode: Option<u32>) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    if let Some(mode) = mode {
        options.mode(mode);
    }
    options.open(temp)
}

/// See the unix arm. Windows carries no mode bits, so the request is dropped;
/// `create_new` is the half that matters here and is portable.
#[cfg(not(unix))]
fn create_exclusive(temp: &Path, _mode: Option<u32>) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
}

/// Set `mode` on an already-open file, undoing any umask narrowing.
#[cfg(unix)]
fn enforce_mode(file: &std::fs::File, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(std::fs::Permissions::from_mode(mode))
}

/// See the unix arm. Windows has no mode bits to enforce.
#[cfg(not(unix))]
fn enforce_mode(_file: &std::fs::File, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// A suffix no two concurrent writers share: process, plus thread within it.
///
/// This is not a secret and must not be relied on as one. The pid is visible in
/// `ps` and `DefaultHasher` is fixed-seed, so the name it builds is guessable
/// by a local attacker — [`write_temp`]'s exclusive open is what makes that
/// harmless (#5868).
fn writer_tag() -> String {
    use std::hash::{Hash as _, Hasher as _};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    format!("{}-{}", std::process::id(), hasher.finish())
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    const HOME: &str = "/Users/recipient";

    #[test]
    fn resolution_order_is_flag_then_env_then_default() {
        let cwd = Path::new("/engagement");
        let home = Some(Path::new(HOME));

        let flag = WorkDir::resolve(Some(PathBuf::from("/flag")), Some("/env"), home, cwd);
        assert_eq!(flag.root(), Path::new("/flag"));

        let env = WorkDir::resolve(None, Some("/env"), home, cwd);
        assert_eq!(env.root(), Path::new("/env"));

        let default = WorkDir::resolve(None, None, home, cwd);
        assert_eq!(
            default.root(),
            Path::new("/Users/recipient/.trusty-tools/trusty-audit/work")
        );
    }

    /// #5915: the default is the one placement `trusty-search` will index. The
    /// cwd is the recipient's unzip location, and every likely one of those —
    /// `/tmp`, `/var/folders`, `~/Downloads`, `~/Desktop`, `~/Documents` — is on
    /// a denylist no flag on tga's path relaxes, so a checkout under the cwd was
    /// refused and the code-analysis leg came back empty.
    ///
    /// The assertion is the placement, not the refusal: the denylist lives in
    /// `trusty-search` and this crate cannot call it. What this pins is that the
    /// default no longer descends from the cwd at all.
    #[test]
    fn the_default_root_is_one_trusty_search_will_index() {
        let home = Path::new(HOME);
        for unzipped_at in [
            "/var/folders/9x/T/taudit-package",
            "/tmp/taudit-package",
            "/Users/recipient/Downloads/trusty-audit",
            "/Users/recipient/Desktop/trusty-audit",
            "/Users/recipient/Documents/trusty-audit",
        ] {
            let resolved = WorkDir::resolve(None, None, Some(home), Path::new(unzipped_at));
            assert_eq!(
                resolved.root(),
                Path::new("/Users/recipient/.trusty-tools/trusty-audit/work"),
                "unzipping at {unzipped_at} still decided the root"
            );
            assert!(
                !resolved.root().starts_with(unzipped_at),
                "the root descends from the unzip location: {}",
                resolved.root().display()
            );
        }
    }

    /// The one case the pre-#5915 cwd-relative default survives for: no home to
    /// put anything under. It is not a good root — it is the only one left.
    #[test]
    fn without_a_home_directory_the_root_falls_back_beside_the_cwd() {
        let resolved = WorkDir::resolve(None, None, None, Path::new("/engagement"));
        assert_eq!(resolved.root(), Path::new("/engagement/trusty-audit-work"));
    }

    /// #5672: a relative choice from either source becomes absolute, because
    /// `run::sweep` hands these paths to a child running in a different cwd.
    #[test]
    fn a_relative_choice_is_anchored_to_the_cwd() {
        let cwd = Path::new("/engagement");
        let home = Some(Path::new(HOME));

        let flag = WorkDir::resolve(Some(PathBuf::from("w")), None, home, cwd);
        assert_eq!(flag.root(), Path::new("/engagement/w"));

        let env = WorkDir::resolve(None, Some("w"), home, cwd);
        assert_eq!(env.root(), Path::new("/engagement/w"));
    }

    #[test]
    fn a_blank_env_value_falls_through_to_the_default() {
        let cwd = Path::new("/engagement");
        let resolved = WorkDir::resolve(None, Some("   "), Some(Path::new(HOME)), cwd);
        assert_eq!(
            resolved.root(),
            Path::new("/Users/recipient/.trusty-tools/trusty-audit/work")
        );
    }

    /// The README and the CLI print this string; the resolver computes a path.
    /// They must name the same place, and nothing else checks that they do.
    #[test]
    fn the_display_string_names_the_path_the_resolver_returns() {
        let home = Path::new(HOME);
        let expanded = DEFAULT_ROOT_DISPLAY.replacen('~', &home.display().to_string(), 1);
        assert_eq!(default_root(Some(home)), PathBuf::from(expanded));
    }

    /// The property the README's "delete the directory" instruction rests on.
    #[test]
    fn every_layout_path_is_inside_the_root() {
        let work = WorkDir::new("/engagement/trusty-audit-work");
        for (area, path) in work.layout() {
            assert!(
                path.starts_with(work.root()),
                "{area:?} escapes the root: {}",
                path.display()
            );
        }
    }

    #[test]
    fn area_names_are_distinct() {
        let mut names: Vec<&str> = Area::ALL.iter().map(|a| a.dir_name()).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two areas share a directory name");
    }

    /// The temporary the rename publishes from must never outlive the write —
    /// a `state/` littered with `.tmp` files is how a reader learns to guess.
    #[test]
    fn an_atomic_write_leaves_no_temporary_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("state/run-progress.toml");

        write_atomically(&target, "complete = false\n").expect("first write");
        write_atomically(&target, "complete = true\n").expect("overwrite");

        assert_eq!(
            std::fs::read_to_string(&target).expect("reads"),
            "complete = true\n"
        );
        let leftovers: Vec<PathBuf> = std::fs::read_dir(tmp.path().join("state"))
            .expect("read state")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// #5868: the file the credential lands in is owner-only, and it gets there
    /// through the same untearable rename as every other state file. Both
    /// halves matter — a 0600 file written non-atomically is still a file a
    /// reader can catch half-written.
    #[cfg(unix)]
    #[test]
    fn a_private_write_is_owner_only_and_still_atomic() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("engagement.toml");

        // Land on an existing world-readable file: the mode must be replaced,
        // not inherited, because that is the shape a hand-edited config has.
        std::fs::write(&target, "openrouter_key = \"\"\n").expect("seed");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        write_private_atomically(&target, "openrouter_key = \"sk-or-v1-x\"\n").expect("write");

        let mode = std::fs::metadata(&target)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        assert_eq!(
            std::fs::read_to_string(&target).expect("reads"),
            "openrouter_key = \"sk-or-v1-x\"\n"
        );
        let leftovers: Vec<PathBuf> = std::fs::read_dir(tmp.path())
            .expect("read dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    /// #5868: the temporary name is predictable — the pid is visible in `ps`
    /// and `DefaultHasher` has a fixed seed over a near-deterministic main
    /// `ThreadId` — so a local attacker can pre-plant a symlink at it. Opening
    /// without `O_EXCL` followed that symlink: the credential landed in the
    /// attacker's file and the config became a symlink pointing there. Mode
    /// 0600 does not help, because it constrains a file at CREATION and this
    /// path created nothing.
    #[cfg(unix)]
    #[test]
    fn a_pre_planted_symlink_never_receives_the_credential() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("engagement.toml");
        std::fs::write(&target, "openrouter_key = \"\"\n").expect("seed target");

        let victim = tmp.path().join("attacker-readable.txt");
        std::fs::write(&victim, "untouched\n").expect("seed victim");

        // The exact temporary this thread's write will choose.
        let planted = target.with_file_name(format!("engagement.toml.{}.tmp", writer_tag()));
        std::os::unix::fs::symlink(&victim, &planted).expect("plant symlink");

        write_private_atomically(&target, "openrouter_key = \"sk-or-v1-secret\"\n")
            .expect("the write replaces the plant rather than following it");

        assert_eq!(
            std::fs::read_to_string(&victim).expect("read victim"),
            "untouched\n",
            "the credential was written through the pre-planted symlink"
        );
        assert!(
            !std::fs::symlink_metadata(&target)
                .expect("stat target")
                .file_type()
                .is_symlink(),
            "the rename published the plant instead of a real file"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read target"),
            "openrouter_key = \"sk-or-v1-secret\"\n"
        );
        let mode = std::fs::metadata(&target)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        assert!(!planted.exists(), "the temporary outlived the write");
    }

    /// The other thing `O_EXCL` refuses is a stale temporary from a writer that
    /// crashed holding this tag. It must be replaced wholesale, never opened
    /// and partially overwritten — a shorter new document would leave the old
    /// tail behind and publish a hybrid.
    #[test]
    fn a_stale_temporary_is_replaced_not_reused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("state/run-progress.toml");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir");

        let stale = target.with_file_name(format!("run-progress.toml.{}.tmp", writer_tag()));
        std::fs::write(&stale, "complete = false\nleftover = \"from the crash\"\n")
            .expect("seed stale");

        write_atomically(&target, "complete = true\n").expect("write");

        assert_eq!(
            std::fs::read_to_string(&target).expect("reads"),
            "complete = true\n"
        );
        assert!(!stale.exists(), "the temporary outlived the write");
    }

    /// A target that cannot be published is an error, never a silent no-op —
    /// the caller has to be able to refuse over it.
    #[test]
    fn an_unpublishable_target_is_an_error_and_leaves_no_temporary() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("state/run-progress.toml");
        // A non-empty directory at the target: the rename cannot replace it.
        std::fs::create_dir_all(&target).expect("mkdir");
        std::fs::write(target.join("occupied"), b"x").expect("write");

        let err = write_atomically(&target, "complete = true\n")
            .expect_err("a directory cannot be replaced by a rename");
        assert!(matches!(err, AuditError::WorkDir { .. }), "{err:?}");
        let leftovers: Vec<PathBuf> = std::fs::read_dir(tmp.path().join("state"))
            .expect("read state")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn create_is_idempotent_and_makes_every_area() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));

        work.create().expect("first create");
        work.create().expect("second create is a no-op");

        for (area, path) in work.layout() {
            assert!(
                path.is_dir(),
                "{area:?} was not created: {}",
                path.display()
            );
        }
    }

    /// A directory this account cannot write in, or `None` when it can write
    /// anyway — running as root defeats the mode, and CI containers do.
    #[cfg(unix)]
    fn unwritable_dir(under: &Path) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = under.join("read-only");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        std::fs::create_dir(dir.join("probe"))
            .is_err()
            .then_some(dir)
    }

    /// 🔴 #5941: an unwritable cwd with no home directory is named, not left to
    /// surface as a bare OS error against a path that was never created.
    ///
    /// This is the exact shape the issue reports: `home: None` sends
    /// [`default_root`] to the cwd-relative fallback, and the operator was
    /// standing somewhere they cannot write. Against `6d937ba66` `create`
    /// reached `create_dir_all` and returned `AuditError::WorkDir` naming
    /// `<cwd>/trusty-audit-work/tools` — a directory that does not exist, which
    /// sends the reader hunting for a path instead of at the cause.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_cwd_is_named_before_anything_is_created() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let Some(cwd) = unwritable_dir(tmp.path()) else {
            return;
        };

        let work = WorkDir::resolve(None, None, None, &cwd);
        assert_eq!(work.root(), cwd.join(DEFAULT_WORKDIR_NAME));

        let err = work.create().expect_err("nothing can be written there");
        let AuditError::WorkDirNotWritable {
            root, tried, env, ..
        } = &err
        else {
            panic!("a raw OS error is what this refusal replaces: {err:?}");
        };
        assert_eq!(root, work.root());
        assert_eq!(tried, &cwd, "the message must name the directory it tried");
        assert_eq!(*env, WORKDIR_ENV);

        // The remedy, and nothing the operator has to go and find.
        let message = err.to_string();
        assert!(message.contains(WORKDIR_ENV), "{message}");
        assert!(message.contains("nothing was written"), "{message}");
        assert!(
            !message.contains(Area::Tools.dir_name()),
            "the message names a directory that was never created: {message}"
        );
    }

    /// The check is a write, so it owes the property every other writer here
    /// owes: a refused root leaves no directory and no probe behind.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_root_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let Some(parent) = unwritable_dir(tmp.path()) else {
            return;
        };

        let work = WorkDir::new(parent.join("work"));
        work.create().expect_err("nothing can be written there");

        let left: Vec<PathBuf> = std::fs::read_dir(&parent)
            .expect("read the directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// And the check itself writes nothing that outlives it.
    #[test]
    fn a_writable_root_passes_the_check_and_leaves_no_probe() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let work = WorkDir::new(tmp.path().join("work"));

        work.writable().expect("a tempdir is writable");

        let left: Vec<PathBuf> = std::fs::read_dir(tmp.path())
            .expect("read the directory")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();
        assert!(left.is_empty(), "the probe outlived the check: {left:?}");
    }
}
