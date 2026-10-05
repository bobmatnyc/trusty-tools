Changed
- A managed session name tm de-duplicates now always gets a two-digit suffix: a taken `tm-localizer` becomes `tm-localizer-02`, then `-03`, matching the `tm-<leaf>-01` serial names, instead of `tm-localizer-2`. Existing names are unchanged and still resolve (#9238).
