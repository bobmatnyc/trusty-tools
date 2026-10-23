Added
- `agents::rule_marker` parses rule-id markers (a whole line `<!-- rule:<id> -->`, the id lowercase words joined by `-` or `/`), and `compose_agent` drops every marker line from each chain link, so a composed agent body carries no test metadata (#10058).
