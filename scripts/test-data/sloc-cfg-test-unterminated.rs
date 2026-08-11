//! Fixture: FAIL-CLOSED case. The test module is never closed (the file is
//! truncated), so no closer is found and nothing is excluded.

pub fn live() -> u8 {
    9
}

#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        assert_eq!(live(), 9);
    }
