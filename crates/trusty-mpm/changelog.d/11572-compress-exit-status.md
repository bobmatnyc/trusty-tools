Fixed
- A single command rewritten through `tm compress` now exits with the command's own status instead of 0, so a failing command still reads as a failure.
