Added

- Every `tm` line naming an agent renders that name in a stable per-agent
  identity color, so parallel agents are tellable apart at a glance: the
  `tm agent list` / `tm agent show` roster, the `tm session breakers` AGENT
  column, the `tm doctor` builder-slot census, and `tm catalog ls`. The color is
  an FNV-1a hash of the name into a fixed eight-entry truecolor palette, so one
  name is always one color across sessions and builds. The palette claims none
  of the reserved slots — green/red/yellow for service status, cyan/yellow for
  agent scope — and `NO_COLOR`, a pipe, or any non-TTY renders byte-identical
  output to before (#4068).
