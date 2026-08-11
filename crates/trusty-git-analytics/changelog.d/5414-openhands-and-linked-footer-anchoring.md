Fixed

- The `openhands` trailer marker no longer classifies a human at the vendor as an
  agent. `\bopenhands\b` matched `Co-authored-by: Simon Rosenberg <simon@openhands.dev>`;
  it now keys on `openhands@all-hands.dev`, `openhands-release-bot`, or `OpenHands Bot`.
  Found by running the marker against a real `All-Hands-AI/OpenHands` clone rather than
  fixtures (#5414).
- The `trusty-mpm` and `Claude Code` footer markers now match the markdown-link form
  (`Generated with [trusty-mpm](...)`), which 14 commits in this repo's own history use
  and the #5249 patterns missed. Catch rate on trusty-tools' history rises from
  91.03% to 91.35%.
