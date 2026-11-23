#!/bin/sh
# Regenerates the converter fixtures from source.md. Needs pandoc and python3.
# All text is written for this project; no third-party document is used.
set -eu
cd "$(dirname "$0")"
pandoc source.md -o survey.docx
pandoc source.md -o survey.pptx
pandoc source.md -s --metadata title="Harbor Lights Survey" -o survey.html
python3 make_pdf.py survey.pdf "Harbor Lights Survey" "" \
  "The harbor survey counted forty two lights along the north pier." \
  "Each light was checked at dusk and again at dawn." "" \
  "Seven lights were dim and three were dark." \
  "The keeper replaced every dark lamp within one week."
python3 make_pdf.py scanned.pdf
python3 make_table_pdf.py tables.pdf "Pier report opens here." -- \
  "Lamp counts follow." "Label;Value" "Lights;42" "Dark;3" -- \
  "Closing notes end here."
python3 make_xlsx.py survey.xlsx
