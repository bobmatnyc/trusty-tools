Breaking
- `Runner` gains the required fields `usage_probe: &dyn ProbeLauncher` and `notifier: &dyn Notifier`, and `Sensors` gains `usage_probe`; `verbs::poll` passes `ScriptProbe` and `Osascript`, and tests pass `RecordingProbe` and `RecordingNotifier` ([#9801](https://github.com/bobmatnyc/trusty-tools/issues/9801))
