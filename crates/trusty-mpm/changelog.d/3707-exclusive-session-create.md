Fixed

- **Two sessions can no longer share one tmux pane.** The managed create path
  ran `tmux new-session -A`, and `-A` attaches to an existing session of that
  name instead of failing — so two creators that computed the same name both
  returned success, and two session records were persisted driving one
  terminal, with no error at any step. The create now runs without `-A`: a
  creator that loses the name gets a refusal, folds that name into its taken
  set, and retries under a fresh one, up to four attempts before failing with a
  name collision. The orphan-reaping guard is armed only after a create that
  actually created something, so a loser can never reap the winner's session
  ([#3707](https://github.com/bobmatnyc/trusty-tools/issues/3707))
