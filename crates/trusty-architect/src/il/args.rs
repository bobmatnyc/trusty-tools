//! A small argv parser for the `datg` and `ilio` verbs.
//!
//! Why: the verbs keep `datg.py`'s and `ilio.py`'s flags exactly so a caller can
//! swap the program name and nothing else; the leaf crate takes no argument
//! parsing dependency.
//! What: [`parse`] splits `args` into positionals and flags per a [`Spec`]: value
//! flags (`--title X` or `--title=X`), boolean flags, and repeatable value flags.
//! An unknown flag or a missing value is an error message, which the CLI prints
//! with exit 2 as argparse does.
//! Test: `parses_values_flags_repeats_and_positionals`, `rejects_unknown_and_missing`.

use std::collections::{HashMap, HashSet};

/// The flags one verb accepts, names without the leading `--`.
#[derive(Default)]
pub struct Spec {
    /// Flags that take one value.
    pub values: &'static [&'static str],
    /// Flags that take none.
    pub flags: &'static [&'static str],
    /// Flags that take a value and may repeat.
    pub repeat: &'static [&'static str],
}

/// The result of [`parse`].
#[derive(Debug, Default)]
pub struct Parsed {
    /// Arguments that are not flags, in order.
    pub pos: Vec<String>,
    vals: HashMap<String, String>,
    flags: HashSet<String>,
    rep: HashMap<String, Vec<String>>,
}

impl Parsed {
    /// The value of `--name`, when given.
    pub fn val(&self, name: &str) -> Option<&str> {
        self.vals.get(name).map(String::as_str)
    }

    /// Whether `--name` was given.
    pub fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    /// Every value of a repeatable `--name`, `None` when never given.
    pub fn repeated(&self, name: &str) -> Option<&[String]> {
        self.rep.get(name).map(Vec::as_slice)
    }

    /// `--name` as an integer (argparse `type=int`).
    ///
    /// # Errors
    /// The argparse message for a non-integer value.
    pub fn int(&self, name: &str) -> Result<Option<i64>, String> {
        match self.val(name) {
            None => Ok(None),
            Some(v) => v.trim().parse::<i64>().map(Some).map_err(|_| {
                format!(
                    "argument --{name}: invalid int value: {}",
                    crate::pystr::py_repr(v)
                )
            }),
        }
    }

    /// `--name` as a float (argparse `type=float`).
    ///
    /// # Errors
    /// The argparse message for a non-number.
    pub fn float(&self, name: &str) -> Result<Option<f64>, String> {
        match self.val(name) {
            None => Ok(None),
            Some(v) => v.trim().parse::<f64>().map(Some).map_err(|_| {
                format!(
                    "argument --{name}: invalid float value: {}",
                    crate::pystr::py_repr(v)
                )
            }),
        }
    }
}

/// Splits `args` per `spec`.
///
/// # Errors
/// A message for an unknown flag or a value flag without a value.
pub fn parse(args: &[String], spec: &Spec) -> Result<Parsed, String> {
    let mut out = Parsed::default();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        let Some(body) = a.strip_prefix("--").filter(|b| !b.is_empty()) else {
            out.pos.push(a.clone());
            continue;
        };
        let (name, inline) = match body.split_once('=') {
            Some((n, v)) => (n, Some(v.to_owned())),
            None => (body, None),
        };
        if spec.flags.contains(&name) && inline.is_none() {
            out.flags.insert(name.to_owned());
            continue;
        }
        let takes_value = spec.values.contains(&name) || spec.repeat.contains(&name);
        if !takes_value {
            return Err(format!("unrecognized arguments: {a}"));
        }
        let value = match inline {
            Some(v) => v,
            None => {
                let v = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| format!("argument --{name}: expected one argument"))?;
                i += 1;
                v
            }
        };
        if spec.repeat.contains(&name) {
            out.rep.entry(name.to_owned()).or_default().push(value);
        } else {
            out.vals.insert(name.to_owned(), value);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    const SPEC: Spec = Spec {
        values: &["title", "rank"],
        flags: &["end"],
        repeat: &["wait"],
    };

    #[test]
    fn parses_values_flags_repeats_and_positionals() {
        let p = parse(
            &v(&[
                "T",
                "--title=a b",
                "--rank",
                "3",
                "--end",
                "--wait",
                "x:y",
                "--wait",
                "z:w",
                "N",
            ]),
            &SPEC,
        )
        .unwrap();
        assert_eq!(p.pos, ["T", "N"]);
        assert_eq!(p.val("title"), Some("a b"));
        assert_eq!(p.int("rank").unwrap(), Some(3));
        assert!(p.flag("end") && !p.flag("other"));
        assert_eq!(p.repeated("wait").unwrap(), ["x:y", "z:w"]);
        assert!(p.repeated("none").is_none());
    }

    #[test]
    fn rejects_unknown_and_missing() {
        assert_eq!(
            parse(&v(&["--nope"]), &SPEC).unwrap_err(),
            "unrecognized arguments: --nope"
        );
        assert_eq!(
            parse(&v(&["--title"]), &SPEC).unwrap_err(),
            "argument --title: expected one argument"
        );
        let p = parse(&v(&["--rank", "x"]), &SPEC).unwrap();
        assert_eq!(
            p.int("rank").unwrap_err(),
            "argument --rank: invalid int value: 'x'"
        );
    }
}
