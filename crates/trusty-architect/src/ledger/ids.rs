//! Ledger codes and ids: the one grammar, defined once (LDG-2.2, #10062 gap G2).
//!
//! Why: the prototype, the dashboard and the store each carried their own id
//! regex; DOC-82 LDG-1.1 wants one definition every caller imports.
//! What: [`Code`] (`^[a-z][a-z0-9]{1,7}$`), [`Prefix`] (the PM ledger's
//! G, D, A, T, B), [`LedgerId`] (`<code>-<Prefix><hex2>`, lowercase hex) and
//! [`is_bare_id`] (`^[A-Z][0-9a-f]{2}$`, the dashboard's bare form). Hand
//! parsers, not regexes: the crate takes no regex dependency (#9171 A1).
//! Test: `ledger_id_grammar_accepts_exactly_the_one_form`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::LedgerError;

/// A per-session id namespace, unique per host (LDG-2.2.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Code(String);

impl TryFrom<String> for Code {
    type Error = LedgerError;

    fn try_from(s: String) -> Result<Self, LedgerError> {
        Self::parse(&s)
    }
}

impl From<Code> for String {
    fn from(code: Code) -> Self {
        code.0
    }
}

impl Code {
    /// Parses `^[a-z][a-z0-9]{1,7}$`.
    ///
    /// # Errors
    /// [`LedgerError::Invalid`] for any other string.
    pub fn parse(s: &str) -> Result<Self, LedgerError> {
        let mut chars = s.chars();
        let first = chars.next().is_some_and(|c| c.is_ascii_lowercase());
        let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if first && rest && (2..=8).contains(&s.len()) {
            Ok(Self(s.to_owned()))
        } else {
            Err(LedgerError::Invalid(format!(
                "code {s:?}: 2-8 chars, lowercase letters/digits, letter first"
            )))
        }
    }

    /// The code text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The record kind an id names in a PM ledger (LDG-2.1.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Prefix {
    /// `G`: goal.
    #[serde(rename = "G")]
    Goal,
    /// `D`: decision (an owner question).
    #[serde(rename = "D")]
    Decision,
    /// `A`: ask (an owner action).
    #[serde(rename = "A")]
    Ask,
    /// `T`: task.
    #[serde(rename = "T")]
    Task,
    /// `B`: brief (#9759).
    #[serde(rename = "B")]
    Brief,
}

impl Prefix {
    /// Every prefix, in `next` key order.
    pub const ALL: [Self; 5] = [
        Self::Goal,
        Self::Decision,
        Self::Ask,
        Self::Task,
        Self::Brief,
    ];

    /// The id letter.
    pub fn letter(self) -> char {
        match self {
            Self::Goal => 'G',
            Self::Decision => 'D',
            Self::Ask => 'A',
            Self::Task => 'T',
            Self::Brief => 'B',
        }
    }

    /// The prefix for `c`, if it is one.
    pub fn from_letter(c: char) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.letter() == c)
    }
}

/// `<code>-<Prefix><hex2>`: a PM-ledger id (LDG-2.2.2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LedgerId {
    code: Code,
    prefix: Prefix,
    number: u8,
}

impl LedgerId {
    /// An id from its parts.
    pub fn new(code: Code, prefix: Prefix, number: u8) -> Self {
        Self {
            code,
            prefix,
            number,
        }
    }

    /// Parses `^[a-z][a-z0-9]{1,7}-[GDATB][0-9a-f]{2}$`.
    ///
    /// # Errors
    /// [`LedgerError::Invalid`] for any other string.
    pub fn parse(s: &str) -> Result<Self, LedgerError> {
        let bad =
            || LedgerError::Invalid(format!("bad id {s:?}; expected <code>-<G|D|A|T|B><hex2>"));
        let (code, tail) = s.split_once('-').ok_or_else(bad)?;
        let code = Code::parse(code).map_err(|_| bad())?;
        let mut chars = tail.chars();
        let prefix = chars.next().and_then(Prefix::from_letter).ok_or_else(bad)?;
        let number = hex2(chars.as_str()).ok_or_else(bad)?;
        Ok(Self::new(code, prefix, number))
    }

    /// The namespace code.
    pub fn code(&self) -> &Code {
        &self.code
    }

    /// The record kind.
    pub fn prefix(&self) -> Prefix {
        self.prefix
    }

    /// The two-hex-digit number.
    pub fn number(&self) -> u8 {
        self.number
    }
}

impl fmt::Display for LedgerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{}{:02x}",
            self.code.as_str(),
            self.prefix.letter(),
            self.number
        )
    }
}

/// True for the bare dashboard form `^[A-Z][0-9a-f]{2}$` (LDG-2.2.1).
pub fn is_bare_id(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase()) && hex2(chars.as_str()).is_some()
}

/// Two lowercase hex digits, as a number.
pub(crate) fn hex2(s: &str) -> Option<u8> {
    let lower_hex = |c: char| c.is_ascii_digit() || ('a'..='f').contains(&c);
    (s.len() == 2 && s.chars().all(lower_hex))
        .then(|| u8::from_str_radix(s, 16).ok())
        .flatten()
}
