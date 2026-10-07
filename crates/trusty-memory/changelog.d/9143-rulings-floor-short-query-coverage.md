Fixed
- The rulings rank floor lifts a ruling on a query of three or fewer content terms only when the ruling holds every term. Before, two incidental matches out of three (for example "live" from "live-check" and "version" from a `--version` flag) lifted an unrelated ruling to rank 3 (#9143).
