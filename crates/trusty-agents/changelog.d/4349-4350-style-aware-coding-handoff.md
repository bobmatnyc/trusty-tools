Added

- **Style-aware coding handoff: `HandoffContext.style` + an addressable coding
  PM (issues [#4349](https://github.com/bobmatnyc/trusty-tools/issues/4349),
  [#4350](https://github.com/bobmatnyc/trusty-tools/issues/4350); spec DOC-62
  §5/§6).** `dispatch_task` gains two things. First, a closed
  `ExecutionStyle` vocabulary — `hack` / `vibe` / `engineer` — that a caller may
  attach to a handoff as a *ceremony request*, resolved caller > `[subagents]
  default_style` > built-in `engineer` and then raised, never lowered, to the
  target lane's own floor. The resolution travels outbound as a policy block
  appended to the existing preamble and inbound on the `ProposalEnvelope`, so a
  caller always learns which style actually ran. Second, `specialist:
  "coding-pm"` names the external coding project manager the coding lane has
  always run, making that route selectable and stylable instead of reachable
  only by accident of task phrasing.

  The style is a request, not a setting: `ResolvedStyle::resolve` is the only
  constructor, combines every input with `max` over a ceremony-ordered enum, and
  exposes no mutator — so no code path yields an effective style below the
  callee's floor. `vibe` is unimplemented ([#2596](https://github.com/bobmatnyc/trusty-tools/issues/2596)),
  so it runs the `engineer` pipeline today and reports effective style
  `engineer` with reason `tier-unimplemented` rather than silently accepting a
  tier that does not exist.

  Nothing about reach changed. `NON_CODING_TARGETS` is untouched and still the
  code-enforced closed literal behind
  [#4126](https://github.com/bobmatnyc/trusty-tools/issues/4126); `coding-pm` is
  deliberately not a member of it and is resolved on its own path, and
  `DispatchTarget::CodingPm` carries no caller string onward, so the coding
  leg's argv is byte-identical to an unnamed coding dispatch. Style is an input
  to advisory preamble text and to nothing else — not the route, the target, the
  argv, the allow-set, or any tool permission. An absent style is byte-identical
  to previous behaviour at every level: no wire key, no policy block, no
  envelope field.

- **`[subagents] default_style` in agent TOML.** The configured middle
  precedence level for coding-delegation ceremony (`hack` / `vibe` /
  `engineer`). Absent falls through to the built-in `engineer`, i.e. today's
  behaviour; an unrecognized value is a config parse error, never a silent
  default.
