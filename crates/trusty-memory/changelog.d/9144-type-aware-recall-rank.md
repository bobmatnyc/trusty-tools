Changed
- Recall ranking reads the drawer type. A `Ruling` or `Decision` drawer counts as a ruling, like a ruling tag, for snapshot demotion and the cross-palace rulings leg. A `Status` drawer's score is multiplied by 0.8 and a `Turn` drawer's by 0.7, on top of any snapshot decay; a drawer holding a live `fact_key` slot is exempt (#9144).
