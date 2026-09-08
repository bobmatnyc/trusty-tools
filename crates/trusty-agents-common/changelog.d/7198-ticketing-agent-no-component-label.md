Changed

- The bundled `ticketing` agent now instructs posting a `no-component-label: <reason>` comment whenever no crate label fits the finding's file path, alongside the existing `no-milestone: <reason>` rule (#7198). Without that comment `tm issue audit` reports the absent component label as a FAIL; with it the row renders SKIP with the reason quoted.
