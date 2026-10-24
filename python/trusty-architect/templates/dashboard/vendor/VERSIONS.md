# Vendored reader libraries

Served by `scripts/dashboard-serve.py` at `/vendor/<file>` (fixed name list). Obtained with `npm pack <name>@<version>`; files copied unmodified.

| File | Package | Version | License | Source file in package |
| --- | --- | --- | --- | --- |
| marked.umd.js | marked | 18.1.0 | MIT | lib/marked.umd.js |
| purify.min.js | dompurify | 3.4.16 | MPL-2.0 OR Apache-2.0 | dist/purify.min.js |

`mermaid.min.js` (mermaid 12.1.0, MIT) is not shipped here. The server serves it from the instance tree when an installation adds it; without it the reader shows "Diagram could not be drawn" and the diagram source.
