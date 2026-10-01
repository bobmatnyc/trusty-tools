//! Tests for the Architect's conversation record (#8981).

use super::*;

/// A scratch `~/.trusty-mpm` root, Architect directory and claude config dir.
struct Scratch {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    dir: PathBuf,
    config: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = std::fs::canonicalize(tmp.path()).expect("canonical tempdir");
        let dir = base.join("arch");
        std::fs::create_dir_all(&dir).expect("architect dir");
        Self {
            root: base.join(".trusty-mpm"),
            config: base.join("claude-config"),
            dir,
            _tmp: tmp,
        }
    }

    /// Claude Code's transcript for conversation `id` run in the Architect dir.
    fn transcript(&self, id: &str) {
        let folder = self.config.join("projects").join(project_folder(&self.dir));
        std::fs::create_dir_all(&folder).expect("project folder");
        std::fs::write(folder.join(format!("{id}.jsonl")), "{}\n").expect("transcript");
    }

    fn resolve(&self) -> ConversationStart {
        resolve_conversation(&self.root, &self.dir, Some(&self.config))
    }
}

const ID: &str = "3f2b8c1e-5d4a-4b6f-9e2d-1a7c0b9e8f61";

#[test]
fn the_project_folder_name_matches_claude_code() {
    assert_eq!(
        project_folder(Path::new(
            "/Users/masa/trusty-mpm-projects/bobmatnyc/supervisor"
        )),
        "-Users-masa-trusty-mpm-projects-bobmatnyc-supervisor"
    );
    assert_eq!(
        project_folder(Path::new("/a/.claude/_b c")),
        "-a--claude--b-c"
    );
}

#[test]
fn the_claude_args_name_the_conversation() {
    assert_eq!(
        ConversationStart::Resume(ID.to_owned()).claude_args(),
        ["--resume".to_owned(), ID.to_owned()]
    );
    let fresh = ConversationStart::Fresh {
        id: ID.to_owned(),
        reason: None,
    };
    assert_eq!(
        fresh.claude_args(),
        ["--session-id".to_owned(), ID.to_owned()]
    );
    assert_eq!(fresh.id(), ID);
}

#[test]
fn no_record_starts_fresh_without_a_reason() {
    let s = Scratch::new();
    match s.resolve() {
        ConversationStart::Fresh { id, reason: None } => {
            assert!(uuid::Uuid::try_parse(&id).is_ok(), "{id}");
        }
        other => panic!("expected a plain fresh start, got {other:?}"),
    }
}

/// #8981 regression: the id comes from tm's own record and Claude Code's
/// transcript, so no daemon session record — deleted, tombstoned or never
/// written — is consulted, and the prior conversation is resumed.
#[test]
fn a_recorded_conversation_with_a_transcript_is_resumed() {
    let s = Scratch::new();
    record_conversation(&s.root, &s.dir, ID).expect("record");
    s.transcript(ID);
    assert_eq!(s.resolve(), ConversationStart::Resume(ID.to_owned()));
    assert!(
        s.resolve()
            .describe()
            .contains(&format!("resuming conversation {ID}"))
    );
}

/// #8981 error arms: each unusable record starts a NEW conversation, never
/// the recorded one, and the summary line names why.
#[test]
fn every_unusable_record_starts_fresh_and_says_why() {
    fn write(s: &Scratch, body: &str) {
        let path = conversation_path(&s.root);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, body).expect("write");
        // Owner-only, whatever the umask: these cases test the content.
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
    fn record_json(dir: &Path, id: &str) -> String {
        serde_json::json!({ "project_dir": dir, "conversation_id": id }).to_string()
    }
    /// A resumable record and transcript, the record at `mode`.
    fn write_mode(s: &Scratch, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        record_conversation(&s.root, &s.dir, ID).expect("record");
        s.transcript(ID);
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(conversation_path(&s.root), perms).expect("chmod");
    }
    type Setup = Box<dyn Fn(&Scratch)>;
    let cases: Vec<(&str, Setup, &str)> = vec![
        ("corrupt", Box::new(|s| write(s, "{not json")), "is corrupt"),
        (
            "unknown field",
            Box::new(|s| {
                write(
                    s,
                    &format!(
                        r#"{{"project_dir":"{}","conversation_id":"{ID}","x":1}}"#,
                        s.dir.display()
                    ),
                )
            }),
            "is corrupt",
        ),
        (
            "another directory",
            Box::new(|s| {
                write(s, &record_json(&s.config, ID));
                s.transcript(ID);
            }),
            "belongs to",
        ),
        (
            "not a uuid",
            Box::new(|s| write(s, &record_json(&s.dir, "--dangerously-skip-permissions"))),
            "not a valid conversation id",
        ),
        (
            "non-canonical uuid",
            Box::new(|s| {
                let upper = ID.to_uppercase();
                write(s, &record_json(&s.dir, &upper));
                s.transcript(&upper);
            }),
            "not a valid conversation id",
        ),
        (
            "missing transcript",
            Box::new(|s| record_conversation(&s.root, &s.dir, ID).expect("record")),
            "has no transcript",
        ),
        // #8981 critic LOW: a record another user could have written.
        (
            "group-writable",
            Box::new(|s| write_mode(s, 0o620)),
            "writable by other users (mode 620)",
        ),
        (
            "world-writable",
            Box::new(|s| write_mode(s, 0o602)),
            "writable by other users (mode 602)",
        ),
        (
            "unreadable",
            Box::new(|s| std::fs::create_dir_all(conversation_path(&s.root)).expect("dir")),
            "could not be read",
        ),
        // #8981 round 2: a resumable record reached through a symlink.
        (
            "symlinked",
            Box::new(|s| {
                write_mode(s, 0o600);
                let path = conversation_path(&s.root);
                let real = path.with_extension("real");
                std::fs::rename(&path, &real).expect("move the record");
                std::os::unix::fs::symlink(&real, &path).expect("symlink");
            }),
            "could not be read",
        ),
    ];
    for (name, setup, want) in cases {
        let s = Scratch::new();
        setup(&s);
        match s.resolve() {
            ConversationStart::Fresh {
                id,
                reason: Some(why),
            } => {
                assert!(why.contains(want), "{name}: {why}");
                assert_ne!(id.to_lowercase(), ID, "{name}: reused the recorded id");
                assert!(uuid::Uuid::try_parse(&id).is_ok(), "{name}: {id}");
                let line = ConversationStart::Fresh {
                    id,
                    reason: Some(why),
                }
                .describe();
                assert!(line.contains("was not resumed"), "{name}: {line}");
            }
            other => panic!("{name}: expected a fresh start with a reason, got {other:?}"),
        }
    }
    // No config dir: the transcript cannot be checked, so no resume.
    let s = Scratch::new();
    record_conversation(&s.root, &s.dir, ID).expect("record");
    s.transcript(ID);
    assert!(matches!(
        resolve_conversation(&s.root, &s.dir, None),
        ConversationStart::Fresh { reason: Some(why), .. } if why.contains("cannot be checked")
    ));
    // An owner-only record still resumes, and clearing it twice is `Ok`.
    write_mode(&s, 0o644);
    assert_eq!(s.resolve(), ConversationStart::Resume(ID.to_owned()));
    clear_conversation(&s.root).expect("clear");
    clear_conversation(&s.root).expect("clear an absent record");
    assert!(matches!(
        s.resolve(),
        ConversationStart::Fresh { reason: None, .. }
    ));
}

/// #8981 round 2: a record another user owns is refused. A test cannot
/// `chown` without root, so it asks for the record as a uid that does not
/// own it, which is the check a real other-owner record meets.
#[test]
fn a_record_owned_by_another_user_is_refused() {
    use std::os::unix::fs::MetadataExt as _;
    let s = Scratch::new();
    record_conversation(&s.root, &s.dir, ID).expect("record");
    let path = conversation_path(&s.root);
    let owner = std::fs::metadata(&path).expect("record").uid();
    assert!(matches!(read_owned_by(&path, owner), Ok(Some(_))));
    let other = owner.wrapping_add(1);
    let why = read_owned_by(&path, other).expect_err("another owner's record was read");
    assert!(
        why.contains(&format!(
            "is owned by uid {owner}, not by this user (uid {other})"
        )),
        "{why}"
    );
}
