Fixed
- The secret gate no longer overflows the stack on a token with thousands of `=`, such as a 100 KB `a=b&a=b&...` query string. Past 64 `=` the token is judged by the entropy rules instead of the key=value rule ([#9748](https://github.com/bobmatnyc/trusty-tools/issues/9748))
