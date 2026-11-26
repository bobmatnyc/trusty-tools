//! Hosted-path repo skills through a fake Contents API reader (#11899 step 2).
//!
//! Why: the webhook review has no checkout; each read is a Contents API call
//! at the base SHA. Each test fails if its rule is reverted — in particular,
//! an HTTP error, a timeout or a rate limit must be a named skip, never
//! "missing" (silent) and never a load.
//! What: a fake [`DocFetcher`] serving scripted `fetch_entry` results and
//! recording every `(path, sha)` it is asked for, driven through
//! [`SkillsCall::resolve`] with `SkillsReader::ContentsAtRef`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use trusty_common::integrity::Sha256Digest;

use super::*;
use crate::integrations::context::contents_at_ref::{
    ContentsEntry, DocFetchError, classify_entry_response,
};
use crate::models::{RepoSkillsRecord, SkipKind};
use crate::pipeline::prompt::build_system_prompt_with_coverage;
use crate::pipeline::prompt_templates::test_support::prompts;

pub(crate) const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"; // pragma: allowlist secret
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"; // pragma: allowlist secret
const CONFIG: &str = ".trusty-review.toml";

type Reply = Result<Option<ContentsEntry>, DocFetchError>;

/// A scripted Contents API: path → reply; an unscripted path is a 404.
#[derive(Default)]
pub(crate) struct Fake {
    replies: HashMap<String, Reply>,
    calls: Mutex<Vec<(String, String)>>,
}

impl Fake {
    fn reply(mut self, path: &str, reply: Reply) -> Self {
        self.replies.insert(path.to_string(), reply);
        self
    }
    fn file(self, path: &str, text: &str) -> Self {
        let entry = ContentsEntry {
            path: path.to_string(),
            kind: "file".to_string(),
            size: text.len(),
            content: Some(text.as_bytes().to_vec()),
        };
        self.reply(path, Ok(Some(entry)))
    }
    pub(crate) fn config(self, names: &[&str]) -> Self {
        let text: String = names
            .iter()
            .map(|n| format!("[[review.skills]]\nname = \"{n}\"\n"))
            .collect();
        self.file(CONFIG, &text)
    }
    pub(crate) fn skill(self, name: &str, body: &str) -> Self {
        self.file(&skill_path(name), &skill_text(name, body))
    }
    pub(crate) fn calls(&self) -> Vec<(String, String)> {
        self.calls.lock().expect("calls").clone()
    }
}

#[async_trait]
impl DocFetcher for Fake {
    async fn fetch(&self, _path: &str, _sha: &str) -> Result<Option<String>, DocFetchError> {
        Err(DocFetchError::Transport(
            "the skills read uses fetch_entry".into(),
        ))
    }
    async fn fetch_entry(&self, path: &str, sha: &str) -> Reply {
        let mut calls = self.calls.lock().expect("calls");
        calls.push((path.to_string(), sha.to_string()));
        self.replies.get(path).cloned().unwrap_or(Ok(None))
    }
}

fn skill_path(name: &str) -> String {
    format!(".claude/skills/{name}/SKILL.md")
}

fn skill_text(name: &str, body: &str) -> String {
    format!("---\nname: {name}\ndescription: Checks things.\n---\n\n{body}\n")
}

fn kinds(record: &RepoSkillsRecord) -> Vec<(String, SkipKind)> {
    let skipped = record.skipped.iter();
    skipped.map(|s| (s.name.clone(), s.kind)).collect()
}

/// Run the hosted reader over `fake` for a PR whose base is [`BASE`].
async fn resolve(fake: Arc<Fake>, fork: bool, base_sha: &str) -> (VoiceConfig, ReviewResult) {
    let mut options = ReviewOptions::default().with_hosted_repo_skills();
    options.doc_fetcher = Some(fake as Arc<dyn DocFetcher>);
    let source = DiffSource::Github {
        owner: "acme".into(),
        repo: "billing".into(),
        pr: 7,
        token: "fixture-token".into(),
    };
    let head = PrHead {
        sha: HEAD.into(),
        fork,
        base_sha: base_sha.into(),
    };
    let mut result = ReviewResult::new("acme", "billing", 7, "t", "");
    let voice = SkillsCall::new(&options, &source, &head, "")
        .resolve(VoiceConfig::default(), &mut result)
        .await;
    (voice, result)
}

/// Brief item 4a, DOC-93 RS-38: a same-repo PR loads the skill named in the
/// base config, read at the base SHA only, and the skill reaches the system
/// prompt; the record carries its name and sha256, the body names it.
#[tokio::test]
async fn hosted_skill_loads_at_base_and_reaches_the_prompt() {
    let fake = Arc::new(
        Fake::default()
            .config(&["billing"])
            .skill("billing", "BASE-RULE"),
    );
    let (voice, result) = resolve(fake.clone(), false, BASE).await;
    let block = voice.repo_skills.clone().expect("a skill block");
    assert!(block.contains("BASE-RULE"), "{block}");
    let system = build_system_prompt_with_coverage(prompts(), &voice, false);
    assert!(system.contains("BASE-RULE"), "{system}");
    let record = result.repo_skills.as_ref().expect("record");
    assert_eq!((record.base_sha.as_str(), record.fork), (BASE, false));
    let want = Sha256Digest::of_bytes(skill_text("billing", "BASE-RULE").as_bytes()).to_string();
    assert_eq!(record.loaded.len(), 1);
    assert_eq!(
        (record.loaded[0].name.as_str(), &record.loaded[0].sha256),
        ("billing", &want)
    );
    let paths = vec![
        (CONFIG.to_string(), BASE.to_string()),
        (skill_path("billing"), BASE.to_string()),
    ];
    assert_eq!(fake.calls(), paths, "only base-SHA reads of the two files");
    assert!(body_notice(Some(record)).contains("- loaded `billing` (sha256 "));
}

/// Brief item 4b, RS-36: a 404 base config is silent, fork or not.
#[tokio::test]
async fn hosted_config_404_is_silent_fork_or_not() {
    for fork in [false, true] {
        let fake = Arc::new(Fake::default());
        let (voice, result) = resolve(fake.clone(), fork, BASE).await;
        assert!(voice.repo_skills.is_none() && result.repo_skills.is_none());
        assert_eq!(fake.calls().len(), 1, "one config read");
    }
}

/// Brief items 3 and 4c, RS-45: every Contents API failure on the config —
/// HTTP 5xx, a timeout, both rate-limit shapes, a base commit the API cannot
/// resolve, no inline content — is one `config_unreadable` skip naming the
/// cause, fork or not. None reads as "absent" (silent).
#[tokio::test]
async fn hosted_config_errors_are_config_unreadable_never_silent() {
    let api = |status: u16, body: &str| {
        Err(DocFetchError::Api {
            status,
            body: body.into(),
        })
    };
    let no_inline = Ok(Some(ContentsEntry {
        path: CONFIG.into(),
        kind: "file".into(),
        size: 10,
        content: None,
    }));
    let cases: Vec<(&str, Reply)> = vec![
        ("returned 502", api(502, "Bad Gateway")),
        (
            "operation timed out",
            Err(DocFetchError::Transport("operation timed out".into())),
        ),
        (
            "rate limited",
            api(403, "API rate limit exceeded for installation"),
        ),
        ("rate limited", api(429, "Too Many Requests")),
        (
            "not readable",
            Err(DocFetchError::MissingCommit("No commit found".into())),
        ),
        ("no inline content", no_inline),
    ];
    for (want, reply) in cases {
        for fork in [false, true] {
            let fake = Arc::new(Fake::default().reply(CONFIG, reply.clone()));
            let (voice, result) = resolve(fake, fork, BASE).await;
            assert!(voice.repo_skills.is_none());
            let record = result
                .repo_skills
                .as_ref()
                .unwrap_or_else(|| panic!("{want}: an error records"));
            assert_eq!(record.fork, fork);
            let skip = &record.skipped[..];
            assert!(
                matches!(skip, [s] if s.name == CONFIG
                    && s.kind == SkipKind::ConfigUnreadable
                    && s.reason.contains("contents API")
                    && s.reason.contains(want)),
                "{want}: {skip:?}"
            );
        }
    }
}

/// Brief item 3, RS-18, RS-25: a skill read failure is a named skip — an
/// HTTP error or a timeout is `unreadable`, a followed symlink and a
/// directory are `unreadable`, and only a 404 is `not_found`.
#[tokio::test]
async fn hosted_skill_errors_are_named_skips() {
    let names = ["s5xx", "stimeout", "smissing", "slink", "sdir"];
    let link = ContentsEntry {
        path: ".claude/shared/SKILL.md".into(),
        kind: "file".into(),
        size: 40,
        content: Some(skill_text("slink", "LINKED").into_bytes()),
    };
    let fake = Fake::default()
        .config(&names)
        .reply(
            &skill_path("s5xx"),
            Err(DocFetchError::Api {
                status: 500,
                body: "boom".into(),
            }),
        )
        .reply(
            &skill_path("stimeout"),
            Err(DocFetchError::Transport("timed out".into())),
        )
        .reply(&skill_path("slink"), Ok(Some(link)))
        .reply(&skill_path("sdir"), Err(DocFetchError::Directory));
    let (voice, result) = resolve(Arc::new(fake), false, BASE).await;
    assert!(voice.repo_skills.is_none(), "nothing loads");
    let record = result.repo_skills.as_ref().expect("record");
    let want: Vec<(String, SkipKind)> = vec![
        ("s5xx".into(), SkipKind::Unreadable),
        ("stimeout".into(), SkipKind::Unreadable),
        ("smissing".into(), SkipKind::NotFound),
        ("slink".into(), SkipKind::Unreadable),
        ("sdir".into(), SkipKind::Unreadable),
    ];
    assert_eq!(kinds(record), want);
    assert!(record.skipped[0].reason.contains("returned 500"));
    assert!(record.skipped[1].reason.contains("timed out"));
}

/// Brief item 4e, RS-23: a skill whose API `size` is over the per-skill cap
/// is an `over_skill_cap` skip even when the API sends no inline content
/// (a file over 1 MB), and the next skill still loads.
#[tokio::test]
async fn hosted_oversize_skill_is_an_over_cap_skip() {
    let big = ContentsEntry {
        path: skill_path("big"),
        kind: "file".into(),
        size: 2 * 1024 * 1024,
        content: None,
    };
    let fake = Fake::default()
        .config(&["big", "small"])
        .reply(&skill_path("big"), Ok(Some(big)))
        .skill("small", "SMALL-RULE");
    let (voice, result) = resolve(Arc::new(fake), false, BASE).await;
    assert!(
        voice
            .repo_skills
            .expect("small loads")
            .contains("SMALL-RULE")
    );
    let record = result.repo_skills.expect("record");
    assert_eq!(kinds(&record), vec![("big".into(), SkipKind::OverSkillCap)]);
    assert!(record.skipped[0].reason.contains("2097152 bytes"));
}

/// Brief item 4d, RS-21, RS-39: a fork PR whose base config lists skills
/// requests the config only, loads no skill text, and records each entry as
/// a `fork` skip; the body says so.
#[tokio::test]
async fn hosted_fork_with_configured_skills_requests_no_skill() {
    let fake = Arc::new(
        Fake::default()
            .config(&["billing"])
            .skill("billing", "BASE-RULE"),
    );
    let (voice, result) = resolve(fake.clone(), true, BASE).await;
    assert!(voice.repo_skills.is_none());
    assert_eq!(fake.calls(), vec![(CONFIG.to_string(), BASE.to_string())]);
    let record = result
        .repo_skills
        .as_ref()
        .expect("a fork of a skills repo records");
    assert!(record.fork && record.loaded.is_empty());
    assert_eq!(kinds(record), vec![("billing".into(), SkipKind::Fork)]);
    assert!(body_notice(Some(record)).contains("comes from a fork"));
}

/// RS-38: no full base SHA, no request. A same-repo PR is silent; a fork
/// still records `fork` (step 1 parity).
#[tokio::test]
async fn hosted_without_valid_base_sha_makes_no_request() {
    for (fork, sha) in [(false, ""), (false, "BBBB"), (true, "")] {
        let fake = Arc::new(Fake::default().config(&["billing"]));
        let (_, result) = resolve(fake.clone(), fork, sha).await;
        assert!(fake.calls().is_empty(), "{sha:?}");
        assert_eq!(result.repo_skills.is_some(), fork);
    }
}

/// The webhook drain's options read skills through the Contents API.
#[test]
fn hosted_review_options_read_through_the_contents_api() {
    let options = crate::webhook_drain::hosted_review_options();
    assert_eq!(options.repo_skills_reader, SkillsReader::ContentsAtRef);
    assert_eq!(
        options.repo_skills,
        Some(RepoSkillsInput::from_repo_config(None))
    );
    let default = ReviewOptions::default();
    assert_eq!(default.repo_skills_reader, SkillsReader::Checkout);
}

/// `classify_entry_response` keeps the API's type, path and size.
#[test]
fn classify_entry_maps_each_response_shape() {
    let file =
        r#"{"type":"file","path":"a/SKILL.md","size":3,"encoding":"base64","content":"YWJj\n"}"#;
    let entry = classify_entry_response(200, file)
        .expect("ok")
        .expect("some");
    assert_eq!(
        (entry.kind.as_str(), entry.path.as_str(), entry.size),
        ("file", "a/SKILL.md", 3)
    );
    assert_eq!(entry.content.as_deref(), Some(&b"abc"[..]));
    let link = r#"{"type":"symlink","path":"a","size":5,"target":"b"}"#;
    let entry = classify_entry_response(200, link)
        .expect("ok")
        .expect("some");
    assert_eq!((entry.kind.as_str(), entry.content), ("symlink", None));
    let huge = r#"{"type":"file","path":"a","size":2000000,"encoding":"none","content":""}"#;
    let entry = classify_entry_response(200, huge)
        .expect("ok")
        .expect("some");
    assert_eq!((entry.size, entry.content), (2_000_000, None));
    assert_eq!(
        classify_entry_response(200, "[]"),
        Err(DocFetchError::Directory)
    );
    assert_eq!(classify_entry_response(404, "{}"), Ok(None));
    assert!(matches!(
        classify_entry_response(404, "No commit found for the ref"),
        Err(DocFetchError::MissingCommit(_))
    ));
    assert!(matches!(
        classify_entry_response(503, "down"),
        Err(DocFetchError::Api { status: 503, .. })
    ));
}
