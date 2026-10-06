Fixed
- UNKNOWN now means only that the reviewer gave no usable answer: its reply
  did not parse (`parse_failed`, with the cause in `error`), or there was no
  reply (`no_reviewer_output`: a call error, an empty or truncated reply, or a
  review stopped before the call). A blocking review whose supporting findings
  were all withheld is REQUEST_CHANGES with `suppressed_reject`, never APPROVE;
  an approving one is APPROVE with `all_withheld`. The reviewer's grade
  counts toward its verdict: an APPROVE graded D or F is a rejection, so with
  every finding withheld it reads REQUEST_CHANGES / `suppressed_reject`, on
  the map-reduce synthesis path too. A rejection whose findings were all
  withheld reads the same even when the gates had approved it, for example
  because every finding was advisory. The PR comment heading
  names any status other than `parsed` beside the verdict (#9310).
- The "N findings withheld" headline counts every withheld finding, read from
  `withheld_findings`, with one line per reason class below it. Each gate used
  to prepend its own count, so the headline could read 6 while the array held
  10 (#9310).
- A Bedrock reply that ended in a tool call is parsed from the tool input
  alone. A ```` ```json ```` fence or a verdict keyword in that text is never
  read, and a tool input that does not deserialize is `parse_failed`, naming
  the serde cause. Text replies keep every parse strategy (#9310).
