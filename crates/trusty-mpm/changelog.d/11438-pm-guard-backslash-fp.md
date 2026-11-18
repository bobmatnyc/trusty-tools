Fixed
- `tm pm-guard` no longer refuses unparseable skill or agent frontmatter whose only backslashes cannot spell a `hooks` key, such as `"C:\Users\me"` or `"match \d+"`. A hex escape (`\x`, `\u`, `\U`) or an escaped line break still fails closed.
