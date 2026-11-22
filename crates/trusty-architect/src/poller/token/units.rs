//! Cost units and per-line facts: `family`, `turn_units`, `effort_of`,
//! `thinking_of` from `token_watch.py`.
//!
//! Why: the fleet rules (`opus_share`, `burn`) and the hourly rollup compare
//! units, so the proxy must match the prototype's.
//! What: [`turn_units`] is price x (in + 5 out + w_read x read + 1.25 c5m +
//! 2 c1h) / 1e6; an unpriced family (`fable`) is 0.
//! Test: `family_fable_is_not_priced_as_sonnet`.

use serde_json::Value;

/// Priced families, in lookup order.
const PRICE: [(&str, f64); 3] = [("opus", 5.0), ("sonnet", 3.0), ("haiku", 1.0)];
/// Recognised families with no price (ar-Te4).
const UNPRICED: [&str; 1] = ["fable"];

/// `family(model)`: the first known family named in the model, else `sonnet`.
pub fn family(model: &str) -> &'static str {
    let m = model.to_lowercase();
    PRICE
        .iter()
        .map(|(k, _)| *k)
        .chain(UNPRICED)
        .find(|k| m.contains(k))
        .unwrap_or("sonnet")
}

/// True for a family with no price.
pub fn unpriced(fam: &str) -> bool {
    UNPRICED.contains(&fam)
}

/// A usage number: an integer, a float truncated, else 0 (Python's `or 0`).
pub fn num(v: Option<&Value>) -> u64 {
    match v {
        Some(Value::Number(n)) => n
            .as_u64()
            .or_else(|| n.as_f64().map(|f| f.max(0.0) as u64))
            .unwrap_or(0),
        _ => 0,
    }
}

/// `turn_units(model, usage)`.
pub fn turn_units(model: &str, u: &Value) -> f64 {
    let fam = family(model);
    let Some(price) = PRICE.iter().find(|(k, _)| *k == fam).map(|(_, p)| *p) else {
        return 0.0;
    };
    let cc = &u["cache_creation"];
    let c1h = num(cc.get("ephemeral_1h_input_tokens")) as f64;
    let c5 = match cc.get("ephemeral_5m_input_tokens") {
        Some(v) if !v.is_null() => num(Some(v)) as f64,
        _ => (num(u.get("cache_creation_input_tokens")) as f64 - c1h).max(0.0),
    };
    let w = if fam == "opus" { 0.05 } else { 0.10 };
    let raw = num(u.get("input_tokens")) as f64
        + 5.0 * num(u.get("output_tokens")) as f64
        + w * num(u.get("cache_read_input_tokens")) as f64
        + 1.25 * c5
        + 2.0 * c1h;
    price * raw / 1e6
}

/// `effort_of(line)`: the top-level effort with `|` as `/`, else `unknown`.
pub fn effort_of(d: &Value) -> String {
    match d.get("effort").and_then(Value::as_str) {
        Some(e) if !e.is_empty() => e.replace('|', "/"),
        _ => "unknown".to_owned(),
    }
}

/// `thinking_of(usage)`: `output_tokens_details.thinking_tokens`, else 0.
pub fn thinking_of(u: &Value) -> u64 {
    u["output_tokens_details"]["thinking_tokens"]
        .as_u64()
        .unwrap_or(0)
}
