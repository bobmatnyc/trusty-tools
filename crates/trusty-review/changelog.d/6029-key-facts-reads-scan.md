Fixed

- The report's Key Facts block no longer renders "No data available" while the
  sweep holds the numbers (#6029). It read only a `--analyze` metrics file, so
  an ordinary run — which always produces a repository scan — left every
  density row empty and the polish pass collapsed the whole block; one real
  engagement showed that line above a scan that had measured 1.5M LoC. LoC,
  file count, and languages now take the same metrics-then-scan precedence the
  per-application Profile table already used. A row whose input is genuinely
  absent states which input is missing — complexity names `--analyze`, author
  count and trajectory name the tga authorship artifact, and the work estimate
  names the upstream effort metric that does not exist yet — instead of
  dropping out of the table and blanking the rows around it.
