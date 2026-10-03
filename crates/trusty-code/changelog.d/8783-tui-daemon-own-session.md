Fixed
- The daemon `tcode tui` auto-starts (`tcode serve --http`) now starts in its own session (`setsid`, Unix), so closing the terminal no longer SIGHUPs it along with the TUI's foreground group. The daemon was already meant to outlive the TUI (#8783).
