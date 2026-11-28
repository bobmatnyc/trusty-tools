Changed
- `dashboard::pyderive::pending` appends `: <trimmed answer_note>` to a plain `answered` event, as it already did for Architect Do (the `_with_note` rule of #10765), so the poller, `pending-answers` and the `dashboard-db` summary show the text the owner typed with "Other". This departs on purpose from the frozen `dashboard_derive.py` (Architect ruling 2026-11-28 17:06Z). (#12029 P2g)
