Fixed
- `tm hook --pm-guard` no longer refuses a Python script that assigns a local variable named `ps` (`ps = cell.paragraphs`); a real `ps e`/`ps -E` environment listing is still refused (#10688).
- `tm hook --pm-guard` now reads a Python argv list such as `subprocess.run(["ps", "eww"])` or `["launchctl", "print", …]` as the command it runs, so a script that dumps a process environment that way is refused (#10688).
