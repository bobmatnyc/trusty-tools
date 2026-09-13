Security

- The chat-attachment XLSX bounds validator now follows the package and workbook
  relationships to every sheet instead of scanning only `xl/worksheets/*.xml`, requires an
  explicit `r` coordinate on each cell, bounds `sharedStrings.xml` allocation hints, and
  rejects a package whose ZIP parts differ only by letter case or slash direction as
  ambiguous — a crafted workbook can no longer reach calamine's range allocation through a
  relationship target outside the conventional prefix (#7655).
