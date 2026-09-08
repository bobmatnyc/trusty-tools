Fixed

- The CAST DD template's Report Metadata table no longer hardcodes `CAST (CAST Software) — CAST Highlight + CAST Imaging` as the Vendor / methodology value. That static string carried no provenance marker, unlike every other row in the same table, and could read as a factual claim that CAST Software's platform produced the analysis — no CAST product is invoked; trusty-analyze/trusty-search did the analysis. The row now renders `{{vendor_methodology}}`, the same self-known, provenance-tagged value the generic technical-DD template already used.
