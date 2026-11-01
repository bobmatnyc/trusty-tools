Fixed
- `tm doctor` output_style now finds the style file in the managed `CLAUDE_CONFIG_DIR/output-styles` tier a tm-launched session reads, not only in `~/.claude/output-styles`, so a supervisor install no longer fails the check. It still fails, naming every tier searched, when no tier holds the file (#10580).
