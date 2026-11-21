//! A fake consent dialog: no test ever shows a real one (issue #11460, Phase 4).
//!
//! Why: the consent flow must be tested without `osascript` or Bob's session.
//! What: [`FakeDialog`] answers with a set [`DialogOutcome`] (or hangs past
//! any timeout), counts calls and keeps the last prompt so a test can read
//! the code an operator would see.
//! Test: `tests/consent_fail_closed.rs`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trusty_computer::app_identity::{AppResolver, ResolvedApp};
use trusty_computer::consent::{ConsentDialog, DialogOutcome, Prompt};

/// What the fake does when asked.
#[derive(Debug, Clone)]
pub enum Answer {
    Say(DialogOutcome),
    /// Sleep this long, then confirm (a dialog that ignores its timeout).
    Hang(Duration),
}

/// The fake dialog.
#[derive(Debug)]
pub struct FakeDialog {
    pub answer: Mutex<Answer>,
    pub prompts: Mutex<Vec<Prompt>>,
}

impl FakeDialog {
    /// A dialog that confirms.
    pub fn confirming() -> Arc<FakeDialog> {
        FakeDialog::answering(Answer::Say(DialogOutcome::Confirmed))
    }

    /// A dialog with `answer`.
    pub fn answering(answer: Answer) -> Arc<FakeDialog> {
        Arc::new(FakeDialog {
            answer: Mutex::new(answer),
            prompts: Mutex::new(Vec::new()),
        })
    }

    /// Change the answer.
    pub fn set(&self, answer: Answer) {
        *self.answer.lock().expect("answer") = answer;
    }

    /// How many dialogs were shown.
    pub fn shown(&self) -> usize {
        self.prompts.lock().expect("prompts").len()
    }

    /// The code in the last dialog.
    pub fn last_code(&self) -> String {
        self.prompts
            .lock()
            .expect("prompts")
            .last()
            .map(|p| p.code.clone())
            .expect("a dialog was shown")
    }
}

impl ConsentDialog for FakeDialog {
    fn confirm(&self, prompt: &Prompt, _timeout: Duration) -> DialogOutcome {
        self.prompts.lock().expect("prompts").push(prompt.clone());
        let answer = self.answer.lock().expect("answer").clone();
        match answer {
            Answer::Say(outcome) => outcome,
            Answer::Hang(d) => {
                std::thread::sleep(d);
                DialogOutcome::Confirmed
            }
        }
    }
}

/// The start time every fake process reports unless set otherwise.
pub const FAKE_START_US: u64 = 1_000_000;

/// A fake pid resolver: every pid is [`super::rig::BUNDLE`] unless [`FakeResolver::set`] says otherwise.
#[derive(Debug, Default)]
pub struct FakeResolver {
    pub apps: Mutex<HashMap<u32, Result<ResolvedApp, String>>>,
}

impl FakeResolver {
    /// Answer `bundle_id` (named `name`) for `pid`.
    pub fn set(&self, pid: u32, bundle_id: &str, name: &str) {
        self.set_started(pid, bundle_id, name, FAKE_START_US);
    }

    /// Answer `bundle_id` (named `name`) started at `started_us` for `pid`.
    pub fn set_started(&self, pid: u32, bundle_id: &str, name: &str, started_us: u64) {
        let app = ResolvedApp {
            bundle_id: bundle_id.to_owned(),
            name: name.to_owned(),
            started_us,
        };
        self.apps.lock().expect("apps").insert(pid, Ok(app));
    }
}

impl AppResolver for FakeResolver {
    fn resolve(&self, pid: u32) -> Result<ResolvedApp, String> {
        self.apps
            .lock()
            .expect("apps")
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| {
                Ok(ResolvedApp {
                    bundle_id: super::rig::BUNDLE.to_owned(),
                    name: "Fixture".to_owned(),
                    started_us: FAKE_START_US,
                })
            })
    }
}
