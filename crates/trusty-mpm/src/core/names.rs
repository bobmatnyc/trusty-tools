//! Human-memorable, lifecycle-managed session naming (re-export facade).
//!
//! Why (SPEC-ONESM-01 / DOC-33): the canonical tmux-session naming rule used to
//! live here, but trusty-agents' `TmManager` creates sessions too and could not
//! depend on `trusty-mpm` to reuse it — so its sessions were named by an
//! independent, unprefixed scheme and orphaned by every reconcile/prune/adopt
//! pass. The rule was hoisted into [`trusty_common::session_naming`] (a crate
//! both managers already depend on). This module is now a thin re-export facade
//! so EVERY existing `crate::core::names::…` caller and test in trusty-mpm keeps
//! compiling and behaving identically — the public surface is unchanged.
//! What: `pub use` re-exports the entire moved module (prefixes,
//! [`is_managed_session_name`], [`name_from_uuid`], [`name_from_dir`], the
//! serial-allocation helpers, and [`SessionNameError`]).
//! Test: the moved unit tests now live in `trusty_common::session_naming`;
//! trusty-mpm's own callers are exercised by `core::session`'s
//! `new_derives_tmux_name*` and the daemon/adopt tests, which resolve every
//! symbol through this facade.
pub use trusty_common::session_naming::*;
