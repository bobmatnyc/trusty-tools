Fixed
- `tm fleet init` no longer ends with only "Architect set up" when it started the Architect but could not bind it to its `claude`: the summary adds "(NOT bound: anchor writes will be denied; see the warning above)". The command still exits 0 ([#8436](https://github.com/bobmatnyc/trusty-tools/issues/8436)).
