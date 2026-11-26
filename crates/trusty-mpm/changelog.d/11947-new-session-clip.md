Fixed

- The `tm ls` new-session dialog sizes its project list to the rows the terminal can draw, so on a terminal under 20 rows the highlighted row stays on screen and the arrow keys scroll the list. Moving inside the list no longer scrolls it; it scrolls only when the highlight crosses the top or bottom edge (#11947).
