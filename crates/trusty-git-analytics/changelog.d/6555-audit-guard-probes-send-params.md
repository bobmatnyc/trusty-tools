Changed
- The audit guards' `analyze.health` and `search.health` frames carry `"params": {}` (#6555). Both sent no `params` at all, which decodes to `Value::Null` and works only because those methods are bound to `NoParams`; binding either to a struct would have turned the omission into a `-32602`, which each guard reads as "the daemon cannot serve the report"
