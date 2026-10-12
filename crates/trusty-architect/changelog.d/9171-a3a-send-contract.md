Changed

- `send_guarded` takes a `SendReading`, which only `input_state_for_send` produces, and a `Keys` value; `classify_input`'s reading no longer type-checks there ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `SystemTmux` bounds every call by a timeout (10 s by default): past it the child is killed and reaped and the call returns `TmuxError::Timeout`; output is drained on threads, so a large stdout cannot deadlock ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- Pane parsing reads every Unicode decimal digit as Python's `re` `\d` does, so `ctx` with Arabic-Indic digits parses as a number ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- The send guard's dim check fails closed on a non-SGR CSI and on C1 controls, which could reset attributes mid-row ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
- `Tmux::run` refuses the `send-keys` verb with `TmuxError::SendKeysRefused`; `Tmux` implementors now write `execute`, which only `send_guarded` and `run` can call (it takes a `Permit` that has no public constructor) ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
