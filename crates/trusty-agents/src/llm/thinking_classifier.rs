//! Qwen3 `/think` vs `/no_think` mode classifier.
//!
//! Why: qwen3 models support `/think` and `/no_think` as special tokens that
//! toggle chain-of-thought on a per-turn basis. The ctrl agent defaults to
//! `/no_think` (set in its system prompt) for conversational speed, but
//! hard-reasoning prompts (math, debugging, architecture) benefit from
//! enabling thinking on that turn only. This module provides a fast, pure,
//! zero-dep classifier callers can use to decide whether to inject `/think`.
//! What: `classify_thinking_mode(prompt)` returns `ThinkingMode::Think` for
//! prompts matching reasoning-intensive heuristics (regex over math,
//! debugging, architecture, multi-step, correctness, planning keywords);
//! returns `NoThink` otherwise. Short prompts (<20 chars) always return
//! `NoThink`. Matching is case-insensitive.
//! Test: Unit tests below cover at least one positive example per category
//! and several negative (conversational) examples.

use once_cell::sync::Lazy;
use regex::Regex;

/// Thinking mode hint for qwen3-style models that recognize `/think` and
/// `/no_think` as in-message special tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingMode {
    /// Prompt benefits from chain-of-thought reasoning.
    Think,
    /// Prompt is conversational / short / simple — keep CoT off for speed.
    NoThink,
}

/// Minimum prompt length (bytes) before we even attempt classification.
/// Short prompts ("ok", "thanks", "what's up?") never need /think.
const SHORT_PROMPT_THRESHOLD: usize = 20;

// One compiled regex per category. (?i) = case-insensitive.
// Each pattern is anchored on word boundaries where possible to reduce
// false positives from substring matches inside larger words.

static RE_MATH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(calculat|comput|solv|equation|formula|proof|theorem|integral|derivative|matrix|algorithm complexity|big.?o)\b",
    )
    .expect("math regex")
});

static RE_DEBUG: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(debug|segfault|panic|deadlock|race condition|memory leak|undefined behavior|root cause|why does this (fail|crash|hang))\b",
    )
    .expect("debug regex")
});

static RE_ARCH: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(architect|design pattern|tradeoff|compare.*approach|which.*better|should (I|we) use|pros.*cons|evaluate.*option)\b",
    )
    .expect("arch regex")
});

static RE_MULTISTEP: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(step.by.step|reasoning|analyze|explain (why|how)|walk.?me.?through|deduce|infer|implication)\b",
    )
    .expect("multistep regex")
});

static RE_CORRECTNESS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(correct(ness)?|verify|prove|is this (safe|correct|right)|will this (work|break)|edge case)\b",
    )
    .expect("correctness regex")
});

static RE_PLANNING: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(plan|roadmap|decompose|break.*down|strategy|approach for|how (to|should) implement|set.?up.+(project|environment|system|workflow)|measure\s+\w+\s+(between|across|over|of)|design (a|the|our|an))\b",
    )
    .expect("planning regex")
});

/// Classify whether a prompt should enable qwen3 chain-of-thought.
///
/// Why: Hard-reasoning turns produce better answers with `/think`; everything
/// else runs faster and cheaper with `/no_think` (the system-prompt default).
/// What: Returns `Think` if any reasoning-category regex matches, else
/// `NoThink`. Prompts shorter than [`SHORT_PROMPT_THRESHOLD`] always
/// short-circuit to `NoThink`.
/// Test: See module-level `tests` below.
pub fn classify_thinking_mode(prompt: &str) -> ThinkingMode {
    let trimmed = prompt.trim();
    if trimmed.len() < SHORT_PROMPT_THRESHOLD {
        return ThinkingMode::NoThink;
    }

    if RE_MATH.is_match(trimmed)
        || RE_DEBUG.is_match(trimmed)
        || RE_ARCH.is_match(trimmed)
        || RE_MULTISTEP.is_match(trimmed)
        || RE_CORRECTNESS.is_match(trimmed)
        || RE_PLANNING.is_match(trimmed)
    {
        ThinkingMode::Think
    } else {
        ThinkingMode::NoThink
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_prompt_classifies_as_think() {
        let prompt = "Can you calculate the integral of sin(x) from 0 to pi for me?";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::Think);
    }

    #[test]
    fn debug_segfault_classifies_as_think() {
        let prompt = "Help me debug this segfault in the user session handler.";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::Think);
    }

    #[test]
    fn architect_microservices_classifies_as_think() {
        let prompt = "How should I architect microservices for a hotel revenue platform?";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::Think);
    }

    #[test]
    fn explain_why_classifies_as_think() {
        let prompt = "Please explain why the authentication middleware fails on retries.";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::Think);
    }

    #[test]
    fn version_query_classifies_as_no_think() {
        let prompt = "what version are you running right now?";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::NoThink);
    }

    #[test]
    fn list_files_classifies_as_no_think() {
        let prompt = "list the files in the current project directory";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::NoThink);
    }

    #[test]
    fn casual_thanks_classifies_as_no_think() {
        let prompt = "ok thanks";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::NoThink);
    }

    #[test]
    fn short_prompt_always_no_think() {
        // Even though "debug" would normally match, the prompt is < 20 chars.
        let prompt = "debug it";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::NoThink);
    }

    #[test]
    fn correctness_prompt_classifies_as_think() {
        let prompt = "Is this implementation correct for all edge cases?";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::Think);
    }

    #[test]
    fn planning_prompt_classifies_as_think() {
        let prompt = "What's the right strategy to decompose this monolith?";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::Think);
    }

    #[test]
    fn project_setup_classifies_as_think() {
        // Real REPL prompt: multi-step project setup is reasoning-worthy.
        let prompt = "let's set up a project ~/Projects/trusty-agents \"trusty-agents\". Let me talk to the PM there.";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::Think);
    }

    #[test]
    fn measure_between_classifies_as_think() {
        // Real REPL prompt: "measure X between Y" implies a procedure to design.
        let prompt = "measure timing between prompt and response";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::Think);
    }

    #[test]
    fn list_command_classifies_as_no_think() {
        // Simple imperative listing — should not trigger /think.
        let prompt = "list all registered MCP services on this host";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::NoThink);
    }

    #[test]
    fn factual_lookup_classifies_as_no_think() {
        // One-shot factual question — no chain-of-thought needed.
        let prompt = "what metro north trains run from Grand Central to Scarsdale around 6pm?";
        assert_eq!(classify_thinking_mode(prompt), ThinkingMode::NoThink);
    }
}
