//! Fixture: FAIL-CLOSED case. The raw string below holds an unmatched `{`, so
//! the module's brace balance never returns to zero and the gate refuses to
//! guess where the module ends. Every line is counted as production.

pub fn parse() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    const BROKEN: &str = r#"{ "unclosed": true "#;

    #[test]
    fn t() {
        assert_eq!(parse(), 1);
    }
}
