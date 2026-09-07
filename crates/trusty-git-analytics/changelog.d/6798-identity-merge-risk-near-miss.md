Fixed

- Alias suggestion reaches two near-miss identity splits it used to pass over in
  silence: a display name that matches only once punctuation and whitespace are
  normalised (`ada.lovelace` beside `Ada Lovelace`, confidence 0.90), and a legacy GitHub
  noreply address carrying no `<id>+` prefix (`login@users.noreply.github.com`).
  Both are HIGH-confidence, so the authorship report raises
  `identity_merge_risk` instead of understating bus factor and top-author share.
  Nothing is merged without an operator confirming it, and two distinct people
  whose names merely resemble each other are still never paired.
