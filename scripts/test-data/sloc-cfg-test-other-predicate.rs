//! Fixture: only the exact literal `#[cfg(test)]` is recognised. Every other
//! cfg predicate is COUNTED, because recognising them means parsing nested
//! cfg expressions and `any(test, …)` genuinely compiles into non-test builds.

#[cfg(all(test, unix))]
mod unix_tests {
    #[test]
    fn t() {
        assert!(true);
    }
}

#[cfg(any(test, feature = "x"))]
mod support {
    pub fn helper() -> u8 {
        3
    }
}

#[cfg(not(test))]
pub fn real() -> u8 {
    4
}
