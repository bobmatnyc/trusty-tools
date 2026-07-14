//! Module comment mentioning a path glob like `/api/v1/control/sessions/*` in prose.
//! Also mentions together/* and fireworks/* provider prefixes.

/// Adds two numbers.
///
/// Path convention note: paths under `/api/v1/*` are control-plane routes.
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub fn sub(a: i32, b: i32) -> i32 {
    a - b
}
