Fixed
- Bedrock cost estimates use the AWS Price List rates (effective 2026-09-01)
  and price a `global.` inference profile at the global rate and a geographic
  profile (`us.`, `eu.`, `ap.`, `jp.`) at the regional rate, 10% higher.
  Sonnet 5.5 and Opus 5.5 are now priced instead of reporting $0. Corrected
  `us.` rates in $/MTok input/output: Haiku 4.5 1.10/5.50 (was 0.80/4.00),
  Sonnet 4.6 and Sonnet 4.5 3.30/16.50 (was 3.00/15.00), Opus 4.8 5.50/27.50
  (was 15.00/75.00).
