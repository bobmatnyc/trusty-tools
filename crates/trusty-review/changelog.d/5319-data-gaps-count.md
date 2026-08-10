Fixed

- The report's `Data gaps:` line now states how many gaps it lists and then lists
  exactly that many. It used to comma-join the labels straight after the colon, so
  a label carrying its template section number rendered as
  `Data gaps: 2. Executive Summary, …` — read as a count of two ahead of sixteen
  names. The count is now the length of the same slice the line joins, and items
  are separated with `;` so a label's own comma or leading section number cannot be
  read as a count or a list boundary (#5319).
