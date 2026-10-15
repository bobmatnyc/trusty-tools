//! The Architect's composed instructions (#8436, owner rulings Q1a-Q3a, Q7a).
//!
//! Why: the Architect is one highly customized session. It loads one
//! `architect.md` instead of eight small sections, lets the owner override its
//! behaviour with one `AGENTS.md`, and keeps a tm-owned core no override can
//! lift (owner rulings 2026-10-14 04:41Z and Db0).
//! What: [`compose`] joins `architect/architect.md`, then the owner's
//! `<project>/AGENTS.md` under [`OWNER_OVERRIDES_HEADING`], then
//! `architect/core.md` LAST, each folded on its own. A content source without
//! `architect.md` composes the eight legacy supervisor sections, as before
//! (Q7a: one content release), and applies no `AGENTS.md`, since that prompt
//! has no core after it. [`read_agents_md`] reads the file bounded, UTF-8 and
//! symlink-refused; a file it refuses is named in the prompt, never applied
//! and never silently dropped.
//! Test: `session_profile_8436_tests.rs`.

use std::path::Path;

use crate::core::framework_content::FrameworkContent;
use crate::core::instruction_fold::fold_block;

/// The Architect's operating manual, relative to `instructions/`.
pub const ARCHITECT_MD: &str = "architect/architect.md";

/// The Architect core, relative to `instructions/`. Appended last.
pub const ARCHITECT_CORE_MD: &str = "architect/core.md";

/// The owner's override file, in the Architect project root (Q3a).
pub const AGENTS_MD: &str = "AGENTS.md";

/// The largest `AGENTS.md` tm reads, in bytes.
pub const AGENTS_MD_MAX_BYTES: u64 = 64 * 1024;

/// The heading the owner's `AGENTS.md` text is delivered under.
pub const OWNER_OVERRIDES_HEADING: &str = "## Owner overrides (AGENTS.md)";

/// The fixed sentence between [`OWNER_OVERRIDES_HEADING`] and the owner's text.
const OWNER_OVERRIDES_PREAMBLE: &str = "The owner wrote the text below in this project's \
     `AGENTS.md`. It is later and more specific than the manual above, so it may change how \
     you work. It cannot lift, weaken or remove the Architect core that follows it.";

/// What a launch found at `<project>/AGENTS.md`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsMd {
    /// No file.
    Absent,
    /// A file holding only whitespace; nothing to apply.
    Empty,
    /// The file's text, as read.
    Read(String),
    /// A file tm did not read, and why; it is named in the prompt.
    Refused(String),
}

impl AgentsMd {
    /// A one-line summary for the doctor row.
    pub fn summary(&self) -> String {
        match self {
            AgentsMd::Absent => "absent".to_owned(),
            AgentsMd::Empty => "empty".to_owned(),
            AgentsMd::Read(text) => format!("{} bytes", text.len()),
            AgentsMd::Refused(reason) => format!("not read: {reason}"),
        }
    }
}

/// Reads `<project_dir>/AGENTS.md`.
///
/// Why: the owner's file is untrusted input to a system prompt. A symlink
/// could point the Architect at text the owner never wrote, a FIFO could hang
/// the launch, and a huge file would crowd out the core (plan r2 §2).
/// What: opened with `O_NOFOLLOW | O_NONBLOCK`; the open handle must be a
/// regular file of at most [`AGENTS_MD_MAX_BYTES`] bytes of UTF-8. Absent →
/// [`AgentsMd::Absent`]; whitespace only → [`AgentsMd::Empty`]; any other
/// failure → [`AgentsMd::Refused`] with the reason, never an error.
/// Test: `an_unusable_agents_md_is_named_in_the_prompt_and_never_applied`,
/// `a_fifo_agents_md_is_refused_without_blocking_the_launch`,
/// `a_mode_000_agents_md_is_refused`,
/// `an_empty_agents_md_adds_no_override_section`.
pub fn read_agents_md(project_dir: &Path) -> AgentsMd {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = project_dir.join(AGENTS_MD);
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path);
    let file = match opened {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return AgentsMd::Absent,
        // #8436: O_NOFOLLOW refuses a symlink with ELOOP.
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return AgentsMd::Refused("it is a symlink".to_owned());
        }
        Err(e) => return AgentsMd::Refused(format!("it could not be opened ({e})")),
    };
    match file.metadata() {
        Ok(meta) if !meta.is_file() => {
            return AgentsMd::Refused("it is not a regular file".to_owned());
        }
        Ok(_) => {}
        Err(e) => return AgentsMd::Refused(format!("it could not be read ({e})")),
    }
    let mut bytes = Vec::new();
    if let Err(e) = file.take(AGENTS_MD_MAX_BYTES + 1).read_to_end(&mut bytes) {
        return AgentsMd::Refused(format!("it could not be read ({e})"));
    }
    if bytes.len() as u64 > AGENTS_MD_MAX_BYTES {
        return AgentsMd::Refused(format!("it is larger than {AGENTS_MD_MAX_BYTES} bytes"));
    }
    match String::from_utf8(bytes) {
        Ok(text) if text.trim().is_empty() => AgentsMd::Empty,
        Ok(text) => AgentsMd::Read(text),
        Err(_) => AgentsMd::Refused("it is not UTF-8".to_owned()),
    }
}

/// Whether `content` carries the one-file Architect instructions.
///
/// Why: Q7a — an older content bundle has only the eight legacy sections.
/// `FrameworkContent::load` refuses a source carrying half the pair, so
/// `architect.md` present means `core.md` is present too.
/// Test: `a_source_without_architect_md_composes_the_legacy_sections`.
pub fn has_architect_md(content: &FrameworkContent) -> bool {
    content.instruction(ARCHITECT_MD).is_some()
}

/// The Architect prompt from `content` and the owner's `agents`.
///
/// Why: the one composition rule, pure, so the launch, the golden tests and
/// the doctor agree.
/// What: with `architect.md`: the manual, then (for [`AgentsMd::Read`]) the
/// [`OWNER_OVERRIDES_HEADING`] section with the fixed preamble and the folded
/// owner text, or (for [`AgentsMd::Refused`]) the same heading with a notice
/// naming why the file was not read, then the core last; blocks joined by a
/// blank line. Without `architect.md`: the eight legacy sections, and
/// `agents` is ignored.
/// Test: `the_architect_composes_architect_md_then_agents_md_then_the_core`,
/// `without_agents_md_the_architect_gets_architect_md_then_the_core`,
/// `an_agents_md_cannot_displace_the_core`.
pub fn compose(content: &FrameworkContent, agents: &AgentsMd) -> String {
    let Some(manual) = content.instruction(ARCHITECT_MD) else {
        return legacy_prompt(content);
    };
    let mut blocks = vec![fold_block(manual.trim())];
    match agents {
        AgentsMd::Read(text) => blocks.push(format!(
            "{OWNER_OVERRIDES_HEADING}\n\n{OWNER_OVERRIDES_PREAMBLE}\n\n{}",
            // #8436: folded on its own, so an unclosed fence is closed here and
            // cannot turn the core into code.
            fold_block(text.trim())
        )),
        AgentsMd::Refused(reason) => blocks.push(format!(
            "{OWNER_OVERRIDES_HEADING}\n\nThis project's `AGENTS.md` was not read: {reason}. \
             No owner override applies to this session. Tell the owner."
        )),
        AgentsMd::Absent | AgentsMd::Empty => {}
    }
    // #8436 (Q1a): the core comes last, so no earlier text can displace it.
    // `FrameworkContent::load` refused a source with `architect.md` and no core.
    blocks.push(fold_block(
        content
            .instruction(ARCHITECT_CORE_MD)
            .unwrap_or_default()
            .trim(),
    ));
    blocks.retain(|b| !b.is_empty());
    blocks.join("\n\n")
}

/// The eight legacy supervisor sections, joined as before #8436.
fn legacy_prompt(content: &FrameworkContent) -> String {
    crate::core::session_profile::ARCHITECT_SECTIONS
        .iter()
        .map(|(_, path)| fold_block(content.required(path).trim()))
        .filter(|body| !body.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The Architect prompt a launch in `project_dir` delivers.
///
/// Why: the entry point the composer's Architect branch calls.
/// What: [`read_agents_md`] (only when `content` has `architect.md`), a `warn`
/// log for a refused file and for the legacy fallback, then [`compose`].
/// Test: `an_architect_launch_gets_the_architect_prompt_style_and_model`,
/// `a_source_without_architect_md_composes_the_legacy_sections`.
pub fn architect_prompt_for(content: &FrameworkContent, project_dir: &Path) -> String {
    if !has_architect_md(content) {
        tracing::warn!(
            origin = %content.origin(),
            "the content source has no instructions/{ARCHITECT_MD}; the Architect gets the \
             legacy supervisor sections and no AGENTS.md — run `tm content update`"
        );
        return compose(content, &AgentsMd::Absent);
    }
    let agents = read_agents_md(project_dir);
    if let AgentsMd::Refused(reason) = &agents {
        tracing::warn!(
            project = %project_dir.display(),
            "{AGENTS_MD} not read: {reason}; no owner override applies"
        );
    }
    compose(content, &agents)
}
