//! The `new:` line of an IL block: `K "title" [goal G] [session=S] ... opts ...`.
//!
//! Why: `ilio apply` turns each `new:` line into an add op. Its grammar is one
//! regex in `ilio.py`; the options and sub-question grammars are two more. The
//! crate takes no `regex` dependency, so each is a small cursor parser that keeps
//! the regex's group order and greedy behaviour.
//! What: [`parse_new`] (the line), [`parse_d_opts`] (`"Label :: desc"*|"..."`),
//! [`parse_subs`] (`key: question [:: detail] [rec=yes|no] || ...`),
//! [`sentence_count`] and [`lists_lettered_choices`] (the D detail rules).
//! Test: `parses_a_full_new_line`, `rejects_misordered_new_lines`,
//! `d_opts_need_a_desc`, `subs_split_key_question_and_rec`, `detail_rules`.

use crate::il::error::{IlResult, invalid};
use crate::il::json::{Json, obj};
use crate::pystr::{is_space, is_word};

/// The groups of the `new:` regex.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NewLine {
    /// `D`, `A` or `T`.
    pub kind: char,
    /// The quoted title.
    pub title: String,
    /// `goal G`.
    pub goal: Option<String>,
    /// The `session=`/`brief=`/`task=`/`multistep` tokens.
    pub toks: Vec<String>,
    /// `done_when="..."`.
    pub done_when: Option<String>,
    /// `detail="..."`.
    pub detail: Option<String>,
    /// `subs="..."`.
    pub subs: Option<String>,
    /// Everything after `opts `.
    pub opts: Option<String>,
}

struct Cur {
    t: Vec<char>,
    i: usize,
}

impl Cur {
    fn ws1(&mut self) -> bool {
        let n = self.t[self.i..]
            .iter()
            .take_while(|c| is_space(**c))
            .count();
        self.i += n;
        n > 0
    }

    fn lit(&mut self, s: &str) -> bool {
        let n = s.chars().count();
        let ok = self
            .t
            .get(self.i..self.i + n)
            .is_some_and(|w| w.iter().copied().eq(s.chars()));
        if ok {
            self.i += n;
        }
        ok
    }

    fn word(&mut self) -> Option<String> {
        let n = self.t[self.i..]
            .iter()
            .take_while(|c| !is_space(**c))
            .count();
        let w: String = self.t[self.i..self.i + n].iter().collect();
        self.i += n;
        (n > 0).then_some(w)
    }

    /// `[^"]+"`: a non-empty run up to the next quote, consuming the quote.
    fn quoted(&mut self) -> Option<String> {
        let n = self.t[self.i..].iter().take_while(|c| **c != '"').count();
        if n == 0 || self.t.get(self.i + n) != Some(&'"') {
            return None;
        }
        let s: String = self.t[self.i..self.i + n].iter().collect();
        self.i += n + 1;
        Some(s)
    }

    /// Runs `f`; on `None` the cursor goes back.
    fn attempt<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        let save = self.i;
        let r = f(self);
        if r.is_none() {
            self.i = save;
        }
        r
    }

    fn keyed(&mut self, key: &str) -> Option<String> {
        self.attempt(|c| {
            (c.ws1() && c.lit(key) && c.lit("=\""))
                .then(|| c.quoted())
                .flatten()
        })
    }
}

fn x_token(w: &str) -> bool {
    w == "multistep"
        || ["session=", "brief=", "task="]
            .iter()
            .any(|p| w.strip_prefix(p).is_some_and(|v| !v.is_empty()))
}

/// The `new:` regex; `None` when the line does not fit.
pub fn parse_new(raw: &str) -> Option<NewLine> {
    let mut c = Cur {
        t: raw.chars().collect(),
        i: 0,
    };
    let kind = *c.t.first().filter(|k| "DAT".contains(**k))?;
    c.i = 1;
    if !c.ws1() || !c.lit("\"") {
        return None;
    }
    let title = c.quoted()?;
    let goal = c.attempt(|c| {
        (c.ws1() && c.lit("goal") && c.ws1())
            .then(|| c.word())
            .flatten()
    });
    let mut toks = Vec::new();
    while let Some(w) = c.attempt(|c| c.ws1().then(|| c.word()).flatten().filter(|w| x_token(w))) {
        toks.push(w);
    }
    let done_when = c.keyed("done_when");
    let detail = c.keyed("detail");
    let subs = c.keyed("subs");
    let opts = c.attempt(|c| {
        (c.ws1() && c.lit("opts") && c.ws1()).then(|| {
            let rest: String = c.t[c.i..].iter().collect();
            c.i = c.t.len();
            (!rest.is_empty()).then_some(rest)
        })?
    });
    (c.i == c.t.len()).then_some(NewLine {
        kind,
        title,
        goal,
        toks,
        done_when,
        detail,
        subs,
        opts,
    })
}

fn head(raw: &str) -> String {
    raw.chars().take(60).collect()
}

/// `parse_d_opts`: `"Label :: desc"*|"Label :: desc"` to `[{l, desc, rec}]`.
///
/// # Errors
/// `Invalid` when an option is not quoted or lacks a description.
pub fn parse_d_opts(text: &str, raw: &str) -> IlResult<Json> {
    let mut c = Cur {
        t: crate::pystr::strip(text).chars().collect(),
        i: 0,
    };
    let mut out = Vec::new();
    while c.i < c.t.len() {
        let parsed = c.attempt(|c| {
            c.ws1();
            c.lit("\"").then_some(())?;
            let label = c.quoted()?;
            let rec = c.lit("*");
            c.ws1();
            (c.lit("|") || c.i == c.t.len()).then_some((label, rec))
        });
        let Some((label, rec)) = parsed else {
            return invalid(format!(
                "new: D opts must be \"Label :: one or two sentences\"*|\"Label :: ...\" (quoted, * marks recommended): {}",
                head(raw)
            ));
        };
        let (l, desc) = label.split_once("::").unwrap_or((label.as_str(), ""));
        let (l, desc) = (crate::pystr::strip(l), crate::pystr::strip(desc));
        if l.is_empty() || desc.chars().count() < 10 {
            let short: String = l.chars().take(40).collect();
            return invalid(format!(
                "new: D option \"{short}\" needs a desc after `::` (one or two sentences); put choices in opts, not detail: {}",
                head(raw)
            ));
        }
        out.push(obj([
            ("l", l.into()),
            ("desc", desc.into()),
            ("rec", rec.into()),
        ]));
    }
    Ok(Json::Arr(out))
}

/// `parse_subs`: the dashboard sub-questions of a D, validated (the ledger does not store them).
///
/// # Errors
/// `Invalid` for a part that is not `key: question [:: detail] [rec=yes|no]`.
pub fn parse_subs(text: &str) -> IlResult<Json> {
    let mut out = Vec::new();
    for part in text.split(" || ") {
        let bad = || {
            invalid(format!(
                "new: sub not in the form `key: question [:: detail] [rec=yes|no]`: {}",
                head(part)
            ))
        };
        let p = crate::pystr::lstrip(part);
        let first = p.chars().next();
        let Some((key, rest)) = p
            .split_once(':')
            .filter(|_| first.is_some_and(|f| f != ':' && !is_space(f)))
        else {
            return bad();
        };
        if !rest.chars().next().is_some_and(is_space) {
            return bad();
        }
        let body = crate::pystr::strip(rest);
        let (b, rec) = match body.rsplit_once(|c: char| is_space(c)) {
            Some((b, last))
                if !crate::pystr::strip(b).is_empty()
                    && (last == "rec=yes" || last == "rec=no") =>
            {
                (crate::pystr::rstrip(b), Some(&last[4..]))
            }
            _ => (body, None),
        };
        if b.is_empty() {
            return bad();
        }
        let (q, d) = b.split_once(" :: ").unwrap_or((b, ""));
        let mut sub = obj([
            ("key", key.into()),
            ("question", crate::pystr::strip(q).into()),
        ]);
        if !crate::pystr::strip(d).is_empty() {
            sub.set("detail", crate::pystr::strip(d));
        }
        if let Some(r) = rec {
            sub.set("rec", r);
        }
        out.push(sub);
    }
    Ok(Json::Arr(out))
}

/// `len(re.findall(r"[.!?](?:\s|$)", text))`.
pub fn sentence_count(text: &str) -> usize {
    let t: Vec<char> = text.chars().collect();
    (0..t.len())
        .filter(|&i| matches!(t[i], '.' | '!' | '?') && t.get(i + 1).is_none_or(|c| is_space(*c)))
        .count()
}

/// `re.search(r"\boption\s+[a-d]\b", text, re.I)`.
pub fn lists_lettered_choices(text: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    (0..t.len()).any(|i| {
        let boundary = i == 0 || !is_word(t[i - 1]);
        if !boundary || !crate::pystr::starts_with_ci(&t[i..], "option") {
            return false;
        }
        let j = i + 6;
        let ws = t[j..].iter().take_while(|c| is_space(**c)).count();
        ws > 0
            && t.get(j + ws)
                .is_some_and(|c| matches!(c.to_ascii_lowercase(), 'a'..='d'))
            && t.get(j + ws + 1).is_none_or(|c| !is_word(*c))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_new_line() {
        let n = parse_new(r#"T "Ship it" goal ar-G01 session=s1 multistep done_when="a; b" detail="why" opts x|y"#).unwrap();
        assert_eq!(
            (n.kind, n.title.as_str(), n.goal.as_deref()),
            ('T', "Ship it", Some("ar-G01"))
        );
        assert_eq!(n.toks, ["session=s1", "multistep"]);
        assert_eq!(
            (
                n.done_when.as_deref(),
                n.detail.as_deref(),
                n.opts.as_deref()
            ),
            (Some("a; b"), Some("why"), Some("x|y"))
        );
        let bare = parse_new(r#"A "t""#).unwrap();
        assert_eq!((bare.goal, bare.toks.len(), bare.opts), (None, 0, None));
    }

    #[test]
    fn rejects_misordered_new_lines() {
        for bad in [
            r#"X "t""#,
            r#"T t"#,
            r#"T "t" detail="d" done_when="w""#,
            r#"T "t" multistepper"#,
            r#"T "t" session="#,
            r#"T "t" goal"#,
            r#"T "t" opts "#,
            r#"T "" "#,
        ] {
            assert!(parse_new(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn d_opts_need_a_desc() {
        let ok = parse_d_opts(
            r#""Yes :: do the first thing now"*|"No :: leave it alone for now""#,
            "raw",
        )
        .unwrap();
        assert_eq!(
            ok.dump(),
            r#"[{"l":"Yes","desc":"do the first thing now","rec":true},{"l":"No","desc":"leave it alone for now","rec":false}]"#
        );
        assert!(
            parse_d_opts(r#""Yes :: short""#, "raw")
                .unwrap_err()
                .to_string()
                .contains("needs a desc")
        );
        assert!(
            parse_d_opts("Yes", "raw")
                .unwrap_err()
                .to_string()
                .contains("must be")
        );
    }

    #[test]
    fn subs_split_key_question_and_rec() {
        let s = parse_subs("#3709: Route it? :: Why it matters. rec=yes || k: plain").unwrap();
        assert_eq!(
            s.dump(),
            r##"[{"key":"#3709","question":"Route it?","detail":"Why it matters.","rec":"yes"},{"key":"k","question":"plain"}]"##
        );
        assert!(parse_subs("nocolon").is_err());
        assert!(parse_subs("k:nospace").is_err());
    }

    #[test]
    fn detail_rules() {
        assert_eq!(sentence_count("One. Two! Three? Four"), 3);
        assert_eq!(sentence_count("Ends."), 1);
        assert_eq!(sentence_count("v1.2 is out"), 0);
        assert!(lists_lettered_choices("see Option A or more"));
        assert!(lists_lettered_choices("option   d"));
        assert!(!lists_lettered_choices("options a"));
        assert!(!lists_lettered_choices("option ab"));
    }
}
