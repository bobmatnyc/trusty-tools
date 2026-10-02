Documentation
- `text::elide_middle`'s stated guarantee now matches its behaviour: the whole-trailing-components promise holds when `…/` plus the last two components fit the width (the prefix costs two columns of the budget), and a width of 0 still yields the one-character `…`. (#8205)
