//! Finding `tmux send-keys` relays in a Bash command (#9167).

/// One `tmux send-keys` call that types text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendKeys {
    /// The `-t` target as written.
    pub target: Option<String>,
    /// The text the call types.
    pub text: String,
}

/// The `tmux send-keys` calls in `command` that type text.
pub fn send_keys_calls(_command: &str) -> Vec<SendKeys> {
    Vec::new()
}
