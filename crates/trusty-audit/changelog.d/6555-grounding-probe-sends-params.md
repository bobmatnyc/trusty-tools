Changed
- The grounding guard's `analyze.health` frame carries `"params": {}` (#6555). It sent no `params` at all, which decodes to `Value::Null` and works only because `analyze.health` is bound to `NoParams`; binding that method to a struct would have turned the omission into a `-32602`, which the guard reads as a degraded daemon and refuses the run on
