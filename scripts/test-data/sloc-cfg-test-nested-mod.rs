//! Fixture: an INDENTED `#[cfg(test)] mod tests` nested inside another module
//! is excluded too — the closer is matched at the attribute's own indent.

pub mod outer {
    pub fn value() -> u32 {
        7
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn value_is_seven() {
            assert_eq!(value(), 7);
        }
    }
}
