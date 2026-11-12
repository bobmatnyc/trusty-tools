Fixed
- `tm hook --pm-guard` judges a script followed by an option its interpreter would read itself: `python3 creds.py -c x` and `bash creds.sh -c x` used to skip the script body and allow.
- `tm hook --pm-guard` names a missing or unresolved script instead of judging a later data file in its place: `python3 missing.py notes.txt` refuses, and `python3 missing.py in.docx` names `missing.py`, not `in.docx`.
- `tm hook --pm-guard` reads an option's value word as the value, not the script: `python3 -W in.docx script.py`, `ruby -wr json s.rb`, `bash -oe pipefail s.sh` and `perl -I lib s.pl` judge the script.
- `tm hook --pm-guard` judges the script after a shell's lone `-` (`bash - creds.sh`, `zsh - creds.sh`) and after zsh's `-O` flag (`zsh -O creds.sh`), which it used to skip.
- `tm hook --pm-guard` judges a script read from stdin after `-` or a shell's `-s`: `bash -s < creds.sh` and `python3 - < creds.py` used to allow.
