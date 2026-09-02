//! Developer identity resolution.

pub mod resolver;
pub mod suggest;

pub use resolver::{IdentityResolver, DEFAULT_SIMILARITY_THRESHOLD};
pub use suggest::{Suggestion, HIGH_CONFIDENCE_CUTOFF};
