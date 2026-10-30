//! The MiniLM tokenizer the sidecar truncates with, shared by the lazy adapter (#10504).
//!
//! Why: the deferred parse splits chunks before the sidecar has answered, so the
//! adapter has no reported window yet. Both sidecars truncate with MiniLM at 256.
//! What: [`minilm_tokenizer`] loads that tokenizer once per process from the
//! fastembed cache; [`split_window`] pairs it with the reported window, or with
//! the tokenizer's own `max_length()` when no window is reported yet. No window
//! is persisted, so a model swap is not followed until #10483.
//! Test: `a_lazy_adapter_assumes_the_minilm_window_before_the_sidecar_reports`.

use std::sync::{Arc, OnceLock};

use trusty_common::embedder::{ChunkError, ChunkTokenizer};

/// The fastembed model both sidecars embed with.
const MINILM_MODEL: &str = "Qdrant/all-MiniLM-L6-v2-onnx";

/// A tokenizer that loaded and the window it implies.
type SplitWindow = Option<(usize, Arc<ChunkTokenizer>)>;

/// The MiniLM tokenizer, loaded once. A cache miss is `Err` and is retried by
/// the next caller, since a model download can land between two parses.
pub(super) fn minilm_tokenizer() -> Result<Arc<ChunkTokenizer>, ChunkError> {
    static TOKENIZER: OnceLock<Arc<ChunkTokenizer>> = OnceLock::new();
    if let Some(t) = TOKENIZER.get() {
        return Ok(Arc::clone(t));
    }
    let loaded = Arc::new(ChunkTokenizer::for_fastembed_model(MINILM_MODEL)?);
    Ok(Arc::clone(TOKENIZER.get_or_init(|| loaded)))
}

/// The window to split at: `reported` when the sidecar has one, else the
/// tokenizer's `max_length()`, else no split (`Ok(None)`).
/// Test: `a_lazy_adapter_assumes_the_minilm_window_before_the_sidecar_reports`.
pub(super) fn split_window(reported: Option<usize>) -> Result<SplitWindow, ChunkError> {
    let tokenizer = minilm_tokenizer()?;
    Ok(pick_window(reported, tokenizer.max_length()).map(|window| (window, tokenizer)))
}

/// A reported window wins; else the tokenizer's maximum length (#10504).
/// Test: `a_reported_window_overrides_the_assumed_one`.
fn pick_window(reported: Option<usize>, tokenizer_max: Option<usize>) -> Option<usize> {
    reported.or(tokenizer_max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reported window overrides the tokenizer's; absent both, no split.
    #[test]
    fn a_reported_window_overrides_the_assumed_one() {
        assert_eq!(pick_window(Some(128), Some(256)), Some(128));
        assert_eq!(pick_window(None, Some(256)), Some(256));
        assert_eq!(pick_window(None, None), None);
    }

    /// With the MiniLM tokenizer in the cache, an unreported window is 256 and a
    /// reported one is kept. A cache miss is an `Err` and leaves nothing to assert.
    #[test]
    fn a_lazy_adapter_assumes_the_minilm_window_before_the_sidecar_reports() {
        match split_window(None) {
            Ok(Some((window, _))) => assert_eq!(window, 256),
            Ok(None) => panic!("the MiniLM tokenizer reports a maximum length"),
            Err(e) => {
                eprintln!("MiniLM tokenizer not in the cache, skipping the load check: {e}");
                return;
            }
        }
        assert_eq!(
            split_window(Some(128)).ok().flatten().map(|(w, _)| w),
            Some(128)
        );
    }
}
