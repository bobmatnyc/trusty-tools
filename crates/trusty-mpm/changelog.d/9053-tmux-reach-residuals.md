Fixed

- The Architect-pane tmux floor in `tm hook --pm-guard` now reads tmux run
  through `watch` and `script`, and refuses tmux reached through GNU
  `parallel` or through `xargs` into a wrapper with no program of its own
  (`xargs env`, `xargs sudo`, `xargs timeout 5`).
- Unparseable text naming `TMUX` in any case is treated as tmux, and a shell,
  Python, Node or Ruby option the guard does not know no longer makes the
  next word read as a script, so text piped into that program is judged.
