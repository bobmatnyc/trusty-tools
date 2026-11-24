Fixed
- The Secrets page at `/tools/secrets/` renders again. A comment in its shell quoted the head-close tag, so the built page carried its script, its stylesheet and the console's API base inside that comment and showed a blank page. The console now also injects the API base before the first head-close tag outside a comment ([#11758](https://github.com/bobmatnyc/trusty-tools/issues/11758)).
