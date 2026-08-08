//! Fixture: `#[cfg(test)]` on an `fn` or a `use` is COUNTED. Only `mod` bodies
//! are excluded — see scripts/lib/sloc_awk.sh for why the item grammar stops
//! there. Put test helpers inside the `#[cfg(test)] mod`.

#[cfg(test)]
pub fn helper() -> u8 {
    5
}

#[cfg(test)]
use std::collections::BTreeMap;

pub fn real() -> u8 {
    6
}
