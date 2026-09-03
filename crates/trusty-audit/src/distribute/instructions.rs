//! The `instructions/` directory the recipient reads first (#5473, #5483).
//!
//! Why: the package used to carry one generated `README.md` at its root naming
//! `add repo`, `run` and `package`. That is the three commands #5825 asked for
//! and less than a recipient needs: it never mentioned the one-shot `audit`
//! verb that already existed, never said `trusty-search` asks for Full Disk
//! Access on macOS, named no log location for a run that failed, and gave the
//! operator no reference for the config keys the package's own
//! `engagement.toml` accepts. So the instructions move into their own directory
//! beside the binary, and the root README becomes a pointer at them — one
//! source of truth rather than two that drift.
//!
//! What: two members. `instructions/README.md` is the numbered sequence the
//! recipient runs, rendered from [`README_TEMPLATE`] with the engagement's own
//! labels substituted. `instructions/engagement.template.toml` is the fully
//! commented config reference, shipped verbatim.
//!
//! Both live as files under `crates/trusty-audit/templates/` rather than as
//! string literals here: recipient-facing prose is reviewed as prose, and a
//! 150-line `format!` argument counts against this file's SLOC cap for no
//! benefit. The placeholders are `{{NAME}}` rather than `{}` because the TOML
//! reference is full of braces that a `format!` would have to escape.
//!
//! Test: `super::distribute_tests`.

use crate::config::EngagementConfig;

use super::{LAUNCHER_NAME, README_NAME};

/// The directory the instructions land in, beside the binary.
pub const INSTRUCTIONS_DIR: &str = "instructions";

/// The instructions themselves.
pub const INSTRUCTIONS_README: &str = "instructions/README.md";

/// The commented config reference.
pub const ENGAGEMENT_TEMPLATE_NAME: &str = "instructions/engagement.template.toml";

/// The recipient's numbered sequence, before substitution.
const README_TEMPLATE: &str = include_str!("../../templates/instructions-README.md");

/// The commented config reference, shipped byte for byte.
///
/// It is a LOADABLE config as well as a reference — required keys present,
/// optional ones commented — so an auditor can copy it, fill in the pins, and
/// hand it straight back to `taudit distribute --config`.
/// Test: `super::distribute_tests::the_shipped_config_reference_is_itself_loadable`.
pub const ENGAGEMENT_TEMPLATE: &str = include_str!("../../templates/engagement.template.toml");

/// What the recipient is told about obtaining the OpenRouter key.
///
/// Why: the two package shapes need opposite paragraphs. A package built with
/// `--prompt-for-key` carries no credential, so the recipient must be told the
/// key arrives out of band and that the first `audit` asks for it. A package
/// with the key baked in must NOT tell them to expect a prompt they will never
/// see.
/// Test: `super::distribute_tests::a_prompt_for_key_package_tells_the_recipient_to_expect_the_prompt`.
fn key_step(prompts_for_key: bool) -> &'static str {
    if prompts_for_key {
        "The first thing it does is ask for the OpenRouter key, on this terminal.\n\
         Your auditor sends you that key separately — it is deliberately not in this\n\
         package. What you type is not echoed, is asked for twice, and is saved into\n\
         `../engagement.toml` readable only by your account. Export\n\
         `OPENROUTER_API_KEY` before you run it and it never asks."
    } else {
        "The OpenRouter key for this engagement is already in `../engagement.toml`,\n\
         so nothing is asked for. Export `OPENROUTER_API_KEY` before you run it to\n\
         use a different one instead."
    }
}

/// What the recipient is told about choosing repositories.
///
/// Why: a package built with `--repos` already declares its target list in
/// `engagement.toml`, and telling that recipient to register repositories one
/// at a time invites them to register the same set twice. The picker is the
/// fallback for a package that shipped no list, not the default.
///
/// #5483: the no-list branch now names both ways to register a repository —
/// `add repo` and pasting `[[targets]]` TOML directly — plus `remove`, so a
/// recipient reading only this step can add, list, and drop a target without
/// hunting through the reference config.
/// Test: `super::distribute_tests::a_package_with_a_repo_list_says_the_targets_are_already_declared`,
/// `super::distribute_tests::the_engagement_setup_step_precedes_install_and_names_the_fields_to_replace`.
fn targets_step(declared_repos: usize) -> String {
    if declared_repos > 0 {
        return format!(
            "Your auditor already listed the {} for this engagement — they are the\n\
             `[[targets]]` entries in `../{config}`, and step 4 audits exactly that\n\
             list without asking you to pick anything. Read them first:\n\
             \n\
             ```sh\n\
             ./{LAUNCHER_NAME} targets\n\
             ```\n\
             \n\
             Add one the list missed with `./{LAUNCHER_NAME} add repo <owner>/<name>`, or\n\
             drop one with `./{LAUNCHER_NAME} remove <owner>/<name>`. Both write through to\n\
             `../{config}`.",
            match declared_repos {
                1 => "repository".to_owned(),
                n => format!("{n} repositories"),
            },
            config = EngagementConfig::FILE_NAME,
        );
    }
    format!(
        "This package ships no repository list, so register each repository you want\n\
         audited. Every one is checked against your GitHub credential before it is\n\
         registered, so a typo is refused here rather than halfway through the audit —\n\
         one command per repository, for example two placeholders:\n\
         \n\
         ```sh\n\
         ./{LAUNCHER_NAME} add repo acme/api\n\
         ./{LAUNCHER_NAME} add repo acme/web\n\
         ```\n\
         \n\
         Or paste `[[targets]]` blocks straight into `../{config}` instead — the exact\n\
         shape `add repo` writes:\n\
         \n\
         ```toml\n\
         [[targets]]\n\
         kind = \"repo\"\n\
         name_with_owner = \"acme/api\"\n\
         \n\
         [[targets]]\n\
         kind = \"repo\"\n\
         name_with_owner = \"acme/web\"\n\
         ```\n\
         \n\
         If you already have the list, put a `repos.txt` beside `../{config}` instead —\n\
         one `owner/name` per line, `#` starts a comment — and the client reads it rather\n\
         than asking. List what is registered with `./{LAUNCHER_NAME} targets`, and drop\n\
         one with `./{LAUNCHER_NAME} remove <owner>/<name>`.",
        config = EngagementConfig::FILE_NAME,
    )
}

/// The instructions, with this engagement's own labels substituted.
///
/// Why: the sequence is the same for every engagement, but four facts are not —
/// which platform the binary was built for, where the work root is, whether a
/// key ships, and whether a repository list does. Substituting rather than
/// branching keeps the prose reviewable as one document.
/// What: [`README_TEMPLATE`] with each `{{NAME}}` replaced. Every placeholder is
/// replaced unconditionally, so a template that grows one this function does not
/// know leaves it visible in the output rather than silently dropping it.
///
/// #5483: "Fill in the engagement" — replacing the placeholder `client`,
/// `engagement`, and `instructions` and registering a repository — now sits at
/// step 2, before install and audit, so the recipient edits the config before
/// running anything that reads it.
/// Test: `super::distribute_tests::the_instructions_name_the_one_shot_verb_and_the_disk_access_prompt`,
/// `super::distribute_tests::a_prompt_for_key_package_tells_the_recipient_to_expect_the_prompt`,
/// `super::distribute_tests::the_engagement_setup_step_precedes_install_and_names_the_fields_to_replace`.
pub fn render_readme(
    config: &EngagementConfig,
    platform_line: &str,
    prompts_for_key: bool,
    declared_repos: usize,
) -> String {
    README_TEMPLATE
        .replace("{{ENGAGEMENT}}", &engagement_label(config))
        .replace("{{PLATFORM_LINE}}", platform_line)
        .replace("{{LAUNCHER}}", LAUNCHER_NAME)
        .replace("{{CONFIG}}", EngagementConfig::FILE_NAME)
        .replace("{{WORK}}", crate::workdir::DEFAULT_ROOT_DISPLAY)
        .replace("{{KEY_STEP}}", key_step(prompts_for_key))
        .replace("{{TARGETS_STEP}}", &targets_step(declared_repos))
}

/// How this engagement is named at the top of both generated documents.
pub fn engagement_label(config: &EngagementConfig) -> String {
    match (&config.client, &config.engagement) {
        (Some(client), Some(label)) => format!("{client} — {label}"),
        (Some(client), None) => client.clone(),
        (None, Some(label)) => label.clone(),
        (None, None) => "unlabelled".to_owned(),
    }
}

/// The root `README.md`: a pointer, not a second copy of the instructions.
///
/// Why: #5483. The root README and `instructions/README.md` said overlapping
/// things, and the root one was the stale half — it walked the recipient
/// through `run` then `package` when the one-shot `audit` verb had existed
/// since #5824, and never mentioned Full Disk Access. Two documents that must
/// agree eventually do not, so only one carries the sequence.
/// What: the engagement label, the platform, and the one command that opens the
/// instructions. Short enough that a recipient reads all of it.
/// Test: `super::distribute_tests::the_root_readme_points_at_the_instructions`.
pub fn render_pointer(config: &EngagementConfig, platform_line: &str) -> String {
    format!(
        "# Audit client — {engagement}\n\
         \n\
         {platform_line}\n\
         \n\
         The instructions are in `{INSTRUCTIONS_DIR}/{README_NAME}`. Read that first —\n\
         it is the whole sequence, in order, with the exact command for each step.\n\
         \n\
         ```sh\n\
         open {INSTRUCTIONS_DIR}/{README_NAME}      # or: less {INSTRUCTIONS_DIR}/{README_NAME}\n\
         ```\n\
         \n\
         `{INSTRUCTIONS_DIR}/engagement.template.toml` documents every key\n\
         `{config_file}` accepts, beside this folder.\n\
         \n\
         Nothing else in this folder needs to be opened. `{config_file}` is plain,\n\
         readable TOML and you are meant to read it; `{launcher}` runs the client\n\
         from wherever you extracted this, with no install step and no `PATH` change.\n",
        engagement = engagement_label(config),
        config_file = EngagementConfig::FILE_NAME,
        launcher = LAUNCHER_NAME,
    )
}
