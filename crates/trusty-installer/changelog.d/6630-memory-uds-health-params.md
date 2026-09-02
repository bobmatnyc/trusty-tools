Fixed

- `stack doctor` (and every other `tctl` health rollup) no longer reports
  `trusty-memory … health:down` against a healthy daemon. The UDS probe built
  its `memory.health` request with no `params` field, which decodes
  server-side as `Value::Null`; `HealthQuery` is a plain derived `Deserialize`
  that refuses `null` however many of its fields carry `#[serde(default)]`, so
  every probe answered with a coded `invalid_params` error that rendered
  `down`. The request now carries an explicit `"params": {}`, matching the fix
  `trusty-console`'s own probe already carries for the identical mismatch
  (#6356) ([#6630](https://github.com/bobmatnyc/trusty-tools/issues/6630)).
