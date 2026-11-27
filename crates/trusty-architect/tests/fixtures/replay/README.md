# Replay fixtures (#12026)

Each `*.json` file is one scrubbed recording written by
`trusty-architect-replay record`. `tests/replay_tool.rs`
(`committed_replay_fixtures_pass_the_refusal_check`) loads every file here, so
a fixture holding a home path or a secret-shaped value fails the suite.

## level_lint

The first end-to-end example: three runs of the Architect's `level_lint.py`,
recorded from `<prototype-dir>/scripts/level_lint.py` and replayed in
`tests/replay_level_lint.rs` against the Rust stand-in
`examples/replay_standin_level_lint.rs`.

Regenerate from a checkout of the prototype (inputs are plain text):

```sh
P=<prototype-dir>/scripts/level_lint.py
printf 'Ran cargo test on src/lib.rs:12 with --no-fail-fast.\nSet RUST_LOG first, then see `poll_once`.\n' > note.md
printf 'Ship it with --force; tmux keeps the pane.\n' > stdin.txt
R=trusty-architect-replay; D=tests/fixtures/replay/level_lint
$R record --out $D/pm-strict-file.json --seed note.md=note.md -- $P --level pm --strict --file note.md
$R record --out $D/unknown-level-stdin.json --stdin-file stdin.txt -- $P --level wizard
$R record --out $D/engineer-quiet.json --seed note.md=note.md -- $P --level engineer --file note.md
```

Check that the Python script and the stand-in both replay clean:

```sh
$R replay $D/pm-strict-file.json $D/unknown-level-stdin.json $D/engineer-quiet.json -- $P
$R replay $D/pm-strict-file.json $D/unknown-level-stdin.json $D/engineer-quiet.json -- <target>/debug/examples/replay_standin_level_lint
```
