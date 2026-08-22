Added

- Code Quality & Architecture gains a deterministic crate-topology table for any
  repository whose manifest declares one (trusty-audit measures it from cargo
  metadata). A summary line states the member count, the internal dependency
  edge count, the cycle verdict, and the most-depended-on crates; the table lists
  up to 15 crates — most depended on and shallowest first, so the shared core
  leads — with each one's direct internal dependencies and inbound count. The
  synthesised paragraph renders above it and is told to comment on that
  structure rather than re-derive one.
- The same facts reach the synthesis prompt as a measured block, and the numbers
  are admitted by the numeric guardrail because they are rendered into the fill
  scope the guardrail reads — so the paragraph may quote a member or inbound
  count without the guardrail rejecting the whole field.
- A repository that declares no topology renders a report byte-identical to one
  produced before this existed: the block is omitted whole, never filled with
  honesty markers, and the prompt gains no heading.
