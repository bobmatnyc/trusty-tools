Fixed
- The dashboard schemas `tm fleet init` seeds now accept the prototype's base-34 goal, task and session ids (`0-9` and `a-z` without `l` and `o`, such as `G1g`), as well as every hex-era id; before, a live id with a letter past `f` failed validation ([#12029](https://github.com/bobmatnyc/trusty-tools/issues/12029))
