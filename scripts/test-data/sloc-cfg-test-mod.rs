//! Fixture: an inline `#[cfg(test)] mod tests { … }` is excluded from the
//! SLOC count (issue #5153). Only the six production lines below count.

pub fn one() -> u32 {
    1
}

pub fn two() -> u32 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_is_one() {
        assert_eq!(one(), 1);
    }

    #[test]
    fn two_is_two() {
        assert_eq!(two(), 2);
    }
}
