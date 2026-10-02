Fixed

- A session the daemon relaunches with `claude --resume <id>` (an operator
  resume or an automatic relaunch) can again clear its own delegation
  records. The daemon records the pane and time before it sends the launch
  line; when the resumed `claude` announces the id over the daemon socket,
  the id is rebound to it, but only if it is the `claude` running in that
  pane and it started after the resume. A sibling's `claude` announcing the
  same id is not bound, and a pane lookup that fails rebinds nothing.
