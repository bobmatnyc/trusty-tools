Fixed
- pm-guard no longer refuses `cat > file <<'EOF'` or `cat >> file` writing a quoted here-document whose body carries a word shaped like a secret file name, such as `print(r.key)` or `rows.append({"id": 1})`. The destination is still checked, and a body that `cat` prints, a shell or interpreter runs, or an unquoted delimiter expands is still scanned (#7833).
