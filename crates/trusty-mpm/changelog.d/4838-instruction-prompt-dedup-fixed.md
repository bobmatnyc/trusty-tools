Fixed

- Agent frontmatter using a YAML block scalar (`description: >` or `|`) is now folded instead of taken literally. Five bundled writing agents — `copyeditor`, `pangram-editor`, `proofreader`, `writer`, `writing-critic` — rendered into the PM prompt as a bare `>`; the same defect would have silently truncated any block-scalar `role:` or `model:`
