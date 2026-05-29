//! Rule loading and types for the classification cascade.
//!
//! Rules are loaded from YAML or JSON files (detected by extension) into a
//! [`types::RuleSet`]. A built-in [`loader::default_rules`] covers
//! conventional commit prefixes and common categories so the cascade works
//! without an external rules file.
//!
//! [`multi_loader`] extends this with multi-file merging and the
//! `repo_categories` fallback tier (#445 batch C).

pub mod loader;
pub mod multi_loader;
pub mod types;

pub use loader::{default_rules, load_rules};
pub use multi_loader::{apply_repo_category_fallback, load_rules_multi, repo_matches};
pub use types::{Rule, RuleSet};
