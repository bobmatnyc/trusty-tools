Added

- New leaf crate `trusty-architect` for the trusty-mpm Architect: a `Tmux` trait with a real `SystemTmux` and a recording `FakeTmux`, exact `%N` pane ids, and `send_guarded`, which refuses with an error instead of sending keys into an input box that holds typed text ([#9171](https://github.com/bobmatnyc/trusty-tools/issues/9171))
