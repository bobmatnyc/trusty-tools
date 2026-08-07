Changed

- The UI's Foundry colour tokens are now generated from
  `docs/design/UI/design-system/tokens.css` into
  `ui/src/lib/styles/foundry-tokens.generated.css` instead of hand-transcribed
  into `ui/src/app.css`. No colour changed — the generated values are identical
  to the hand-written ones, and CI fails if the two ever disagree (#5095).
