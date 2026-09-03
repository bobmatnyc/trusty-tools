Fixed

- Report prose follows the rendered data and the recorded provenance
  - The synthesis digest now states the complexity distribution in the same
    string the Code Quality table renders, and a Code Quality paragraph claiming
    no complexity data was provided is dropped with a Synthesis Status
    disclosure when the table renders one (#6691)
  - The digest no longer reports a metrics artifact's zero file/function counts
    as measurements; it falls through to the repository scan, as the Key Facts
    block already did (#6691)
  - The CAST report's §1 "Analysis methodology" row is generated per run from
    what actually contributed — trusty-analyze metrics per application and
    trusty-search trace anchors — instead of fixed template text asserting both
    tools were used (#6675)
