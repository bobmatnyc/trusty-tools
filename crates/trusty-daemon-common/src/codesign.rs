//! Developer ID signing of the binaries an `install-service` verb registers.
//!
//! Why: an ad-hoc `cargo install` binary gets a new code identity on every
//! rebuild. A Keychain access list or a launchd job that names it then breaks:
//! the next Keychain read prompts, and a prompt under launchd hangs the broker.
//! A binary signed with the team-`4JH68XUHC5` Developer ID and a fixed
//! identifier keeps one identity across rebuilds (owner ruling "Sign with
//! Developer ID").
//! What: a [`SignSpec`] names one product's identifier, team, identity env var,
//! `--no-sign` warning and MCP sibling. [`SignSpec::ensure_signed`] checks a
//! binary against [`SignSpec::requirement`], signs it in place when it fails,
//! and verifies again; every failure is an error the caller returns before it
//! writes a plist or manifest. `codesign` and `security` sit behind
//! [`Codesign`] so tests never sign or read the Keychain.
//! Test: `detect_identity_needs_exactly_one_team_identity`,
//! `ensure_signed_signs_then_verifies`, `ensure_install_signed_needs_the_sibling`,
//! plus the signing tests in each consumer's `tests/install.rs`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The Developer ID team both products sign with.
pub const TEAM: &str = "4JH68XUHC5";

/// One product's signing identity and install shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignSpec {
    /// The fixed code-signing identifier (`com.trusty.<product>`).
    pub identifier: &'static str,
    /// The Developer ID team the identity must belong to.
    pub team: &'static str,
    /// Env var naming the signing identity when `--sign-identity` is absent.
    pub identity_env: &'static str,
    /// The stderr warning `--no-sign` prints.
    pub no_sign_warning: &'static str,
    /// File name of the MCP binary `cargo install` puts beside the main one.
    pub mcp_binary: &'static str,
}

/// Runs `codesign` / `security`; a seam so tests never sign or read the Keychain.
pub trait Codesign {
    /// True when `path` satisfies the product's designated requirement.
    fn verify(&self, path: &Path) -> io::Result<bool>;
    /// Sign `path` in place with `identity`; `Err` carries codesign's stderr.
    fn sign(&self, path: &Path, identity: &str) -> io::Result<Result<(), String>>;
    /// `security find-identity -v -p codesigning` stdout.
    fn identities(&self) -> io::Result<String>;
}

/// The real `/usr/bin/codesign` and `/usr/bin/security` for one [`SignSpec`].
#[derive(Debug, Clone, Copy)]
pub struct SystemCodesign {
    /// The product whose requirement and identifier are used.
    pub spec: &'static SignSpec,
}

impl Codesign for SystemCodesign {
    fn verify(&self, path: &Path) -> io::Result<bool> {
        let out = Command::new("/usr/bin/codesign")
            .arg("-v")
            .arg(format!("-R={}", self.spec.requirement()))
            .arg(path)
            .output()?;
        Ok(out.status.success())
    }

    fn sign(&self, path: &Path, identity: &str) -> io::Result<Result<(), String>> {
        let out = Command::new("/usr/bin/codesign")
            .args([
                "--force",
                "--options",
                "runtime",
                "--identifier",
                self.spec.identifier,
            ])
            .args(["-s", identity])
            .arg(path)
            .output()?;
        Ok(if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
        })
    }

    fn identities(&self) -> io::Result<String> {
        let out = Command::new("/usr/bin/security")
            .args(["find-identity", "-v", "-p", "codesigning"])
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// How the install verbs treat signing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signing {
    /// Sign when needed; `None` auto-detects the identity.
    Sign(Option<String>),
    /// `--no-sign`: register the binary as it is, with a warning.
    Skip,
}

/// The signing policy plus the tool that carries it out.
pub struct Signer<'a> {
    /// What to do.
    pub signing: Signing,
    /// How to do it.
    pub tool: &'a dyn Codesign,
}

impl SignSpec {
    /// The designated requirement a registered binary must satisfy.
    pub fn requirement(&self) -> String {
        format!(
            "identifier \"{}\" and anchor apple generic and certificate leaf[subject.OU] = \"{}\"",
            self.identifier, self.team
        )
    }

    /// The one valid `Developer ID Application: … (<team>)` identity in `listing`.
    ///
    /// Why: auto-detection must never guess between two identities.
    /// What: collects the quoted names of `security find-identity` output that
    /// are Developer ID Application identities of the team; exactly one is `Ok`.
    /// Test: `detect_identity_needs_exactly_one_team_identity`.
    pub fn detect_identity(&self, listing: &str) -> Result<String, String> {
        let (team, env) = (self.team, self.identity_env);
        let suffix = format!("({team})");
        let mut found: Vec<&str> = listing
            .lines()
            .filter_map(|l| l.split('"').nth(1))
            .filter(|n| n.starts_with("Developer ID Application: ") && n.ends_with(&suffix))
            .collect();
        found.dedup();
        match found.as_slice() {
            [one] => Ok((*one).to_owned()),
            [] => Err(format!(
                "No Developer ID Application identity for team {team} found; pass --sign-identity, set {env}, or use --no-sign"
            )),
            _ => Err(format!(
                "More than one Developer ID Application identity for team {team}; pass --sign-identity or set {env}"
            )),
        }
    }

    /// Make `exe` satisfy [`SignSpec::requirement`] before it is registered; fail closed.
    ///
    /// Why: registration must not happen unless signing held.
    /// What: `Skip` warns and returns. Otherwise an already-satisfying binary is
    /// left alone; else the identity (given, or [`SignSpec::detect_identity`])
    /// signs `exe` in place and `exe` is verified again. Every failure is `Err`.
    /// Test: `ensure_signed_signs_then_verifies`.
    pub fn ensure_signed(&self, exe: &Path, signer: &Signer<'_>) -> io::Result<()> {
        let identity = match &signer.signing {
            Signing::Skip => {
                eprintln!("{}", self.no_sign_warning);
                return Ok(());
            }
            Signing::Sign(identity) => identity,
        };
        let tool = signer.tool;
        if tool.verify(exe)? {
            return Ok(());
        }
        let identity = match identity {
            Some(i) => i.clone(),
            None => self
                .detect_identity(&tool.identities()?)
                .map_err(io::Error::other)?,
        };
        if let Err(msg) = tool.sign(exe, &identity)? {
            return Err(io::Error::other(format!(
                "codesign failed for {}: {msg}",
                exe.display()
            )));
        }
        if !tool.verify(exe)? {
            return Err(io::Error::other(format!(
                "{} does not satisfy the signing requirement after signing with {identity:?}; nothing was registered",
                exe.display()
            )));
        }
        Ok(())
    }

    /// The MCP binary in the same directory as `exe`.
    pub fn mcp_sibling(&self, exe: &Path) -> PathBuf {
        exe.with_file_name(self.mcp_binary)
    }

    /// Sign `exe` and its [`SignSpec::mcp_sibling`] before an install registers anything.
    ///
    /// Why: the MCP client runs the sibling and cannot answer a prompt (#11440).
    /// What: `Skip` warns once and returns. Otherwise a missing sibling is `Err`;
    /// then [`SignSpec::ensure_signed`] runs on `exe` and on the sibling with
    /// the same identity, and the first failure is returned.
    /// Test: `ensure_install_signed_needs_the_sibling`.
    pub fn ensure_install_signed(&self, exe: &Path, signer: &Signer<'_>) -> io::Result<()> {
        if signer.signing == Signing::Skip {
            return self.ensure_signed(exe, signer);
        }
        let sibling = self.mcp_sibling(exe);
        if !sibling.is_file() {
            return Err(io::Error::other(format!(
                "{} is missing; install both binaries from the same `cargo install`, or use --no-sign",
                sibling.display()
            )));
        }
        self.ensure_signed(exe, signer)?;
        self.ensure_signed(&sibling, signer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const SPEC: SignSpec = SignSpec {
        identifier: "com.trusty.example",
        team: TEAM,
        identity_env: "EXAMPLE_SIGN_IDENTITY",
        no_sign_warning: "warning: --no-sign",
        mcp_binary: "example-mcp",
    };
    const ID: &str = "Developer ID Application: Example (4JH68XUHC5)";

    /// Unsigned until `sign` runs; records every call.
    struct Fake {
        signed: RefCell<bool>,
        calls: RefCell<Vec<String>>,
    }

    impl Codesign for Fake {
        fn verify(&self, path: &Path) -> io::Result<bool> {
            self.calls
                .borrow_mut()
                .push(format!("verify {}", path.display()));
            Ok(*self.signed.borrow())
        }
        fn sign(&self, path: &Path, identity: &str) -> io::Result<Result<(), String>> {
            self.calls
                .borrow_mut()
                .push(format!("sign {} {identity}", path.display()));
            *self.signed.borrow_mut() = true;
            Ok(Ok(()))
        }
        fn identities(&self) -> io::Result<String> {
            Ok(format!("  1) ABC \"{ID}\"\n"))
        }
    }

    fn fake() -> Fake {
        Fake {
            signed: RefCell::new(false),
            calls: RefCell::new(Vec::new()),
        }
    }

    #[test]
    fn requirement_names_identifier_and_team() {
        assert_eq!(
            SPEC.requirement(),
            "identifier \"com.trusty.example\" and anchor apple generic and certificate leaf[subject.OU] = \"4JH68XUHC5\""
        );
    }

    #[test]
    fn detect_identity_needs_exactly_one_team_identity() {
        let one = format!("  1) A \"{ID}\"\n  2) A \"{ID}\"\n");
        assert_eq!(SPEC.detect_identity(&one).as_deref(), Ok(ID));
        let none = SPEC.detect_identity("  1) B \"Apple Development: X (OTHER)\"\n");
        assert!(none.expect_err("none").contains("EXAMPLE_SIGN_IDENTITY"));
        let two = format!("{one}  3) C \"Developer ID Application: Other (4JH68XUHC5)\"\n");
        assert!(SPEC.detect_identity(&two).is_err());
    }

    #[test]
    fn ensure_signed_signs_then_verifies() {
        let tool = fake();
        let signer = Signer {
            signing: Signing::Sign(None),
            tool: &tool,
        };
        SPEC.ensure_signed(Path::new("/x/bin"), &signer)
            .expect("signed");
        assert_eq!(
            *tool.calls.borrow(),
            [
                "verify /x/bin",
                &format!("sign /x/bin {ID}"),
                "verify /x/bin"
            ]
        );
    }

    #[test]
    fn ensure_install_signed_needs_the_sibling() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let exe = tmp.path().join("example");
        std::fs::write(&exe, b"").expect("exe");
        let tool = fake();
        let signer = Signer {
            signing: Signing::Sign(Some(ID.to_owned())),
            tool: &tool,
        };
        let err = SPEC
            .ensure_install_signed(&exe, &signer)
            .expect_err("missing");
        assert!(err.to_string().contains("example-mcp is missing"));
        assert!(tool.calls.borrow().is_empty());
        std::fs::write(SPEC.mcp_sibling(&exe), b"").expect("sibling");
        SPEC.ensure_install_signed(&exe, &signer).expect("both");
        let skip = Signer {
            signing: Signing::Skip,
            tool: &tool,
        };
        std::fs::remove_file(SPEC.mcp_sibling(&exe)).expect("rm");
        SPEC.ensure_install_signed(&exe, &skip)
            .expect("skip ignores the sibling");
    }
}
