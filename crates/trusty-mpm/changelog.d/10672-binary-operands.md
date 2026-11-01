Fixed
- `tm hook --pm-guard` judges only the program a command runs: a data file passed to a script (`python3 "$S" in.docx`, `python3 missing.py in.docx`, `python3 missing.py < in.docx`) is no longer read as the script body. The refusal now names the script that could not be resolved, and `python3 missing.py notes.txt`, which used to allow by judging the text file, refuses.
- `tm hook --pm-guard` judges a script followed by an option its interpreter would read itself: `python3 creds.py -c x` and `bash creds.sh -` used to skip the script body.
