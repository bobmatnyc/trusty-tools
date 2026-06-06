//! Native git operations module (#247).
//!
//! Why: Agents (ctrl, pm, research, observe) need first-class git tools
//! without shelling out for every read. `git2` (libgit2 bindings) gives us
//! safe, fast, cross-platform read access (status, log, branches) with no
//! subprocess overhead. Write operations (commit, push, checkout) shell out
//! to the `git` CLI to preserve hooks, GPG signing, credential helpers, and
//! any user-side `[alias]`/`[core]` configuration that libgit2 does not
//! honor by default.
//! What: Submodules expose typed APIs for each capability area; tools in
//! `crate::tools::git_tools` adapt them to the LLM `ToolExecutor` surface.
//! Test: Each submodule has unit tests against the trusty-agents repo itself
//! (this is run inside a git working tree).

pub mod branch;
pub mod commit;
pub mod log;
pub mod remote;
pub mod repo;
pub mod stash;
pub mod status;

pub use repo::GitRepo;
