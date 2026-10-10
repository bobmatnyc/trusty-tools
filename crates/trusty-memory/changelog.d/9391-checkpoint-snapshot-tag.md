Fixed
- Recall now treats an unkeyed drawer tagged `checkpoint` as a session snapshot: a stale one loses score by age like a `status` drawer, composing with the Status/Turn type factor, and a live fact key still exempts it. Tag matching stays exact (#9391).
