Changed
- `tm tui`'s MPM pane shows a session's details under its row in the sessions column instead of in a popup. Enter, Right or a click on the row expands it; Enter, Left, Esc or another click collapses it, and several rows can stay open across polls. The action bar lists the keys the popup showed (Refs [#10754](https://github.com/bobmatnyc/trusty-tools/issues/10754))
- `tm tui`'s sessions column keeps one order between polls: managers before their workers, then by session name, then by id (Refs [#10754](https://github.com/bobmatnyc/trusty-tools/issues/10754))
