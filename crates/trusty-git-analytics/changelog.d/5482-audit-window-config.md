Added

- `tga audit` reads its lookback window from a new `audit.window_weeks` config
  field, defaulting to 52 weeks when nothing declares one. `--weeks` still wins,
  and the collect, classify, and pr-metrics stages are handed one resolved value
  rather than each applying its own fallback. Previously the window was reachable
  only through `--weeks`, so `trusty-audit` — which spawns `tga audit` without
  it — collected unbounded history (#5482).
