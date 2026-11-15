Fixed
- The architect-dashboard skill (r-183) tells the Architect to read a saved owner answer back with `pending-answers.py --read-back` before acting on it, to relay the stored text, to stop on any non-zero exit, to pass the printed version integer to `--if-version`, and to set `done` only after the `relayed` write succeeds (#10680).
