Fixed
- Closing an answered Architect dashboard item no longer overwrites the text the owner typed: the relay is recorded in a new schema-allowed `relayed` field, the r-183 close rule says so, and `dashboard-db.py` refuses (exit 2) any set, update or batch write that changes a stored `answer` or `answer_note` unless it reopens the item (#10711).
