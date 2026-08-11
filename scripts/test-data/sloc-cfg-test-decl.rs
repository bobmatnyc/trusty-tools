//! Fixture: `#[cfg(test)] mod tests;` declares a SIBLING FILE, not an inline
//! body. Nothing is excluded; behaviour is unchanged from before #5153.

pub fn thing() -> bool {
    true
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "sloc-cfg-test-elsewhere.rs"]
mod elsewhere;
