Fixed

- The CAST template (`report-technical-dd-cast`) renders the ticketing-coverage
  section the generic template gained at 0.17.0. A CAST engagement whose
  manifest declared a valid `[report].ticketing` artifact showed no coverage
  figures and no gap line either: with no section in the template there was no
  placeholder to collapse, so the omission escaped the polish pass that exists
  to name absences. The section is now section 8 in both templates and Gaps &
  Caveats is section 9 in both.
