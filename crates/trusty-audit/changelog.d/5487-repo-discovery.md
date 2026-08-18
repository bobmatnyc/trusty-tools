Added

- `trusty-audit discover` lists every repository the recipient's `gh` credential
  can reach — their own repositories plus each organization the account belongs
  to, not one named org. Every `gh` call routes through `trusty-common`'s single
  entry point, and every failure is a refusal naming the owner that could not be
  listed, never a silently shorter list (#5487, DOC-68 §6 / §14 Q4).
