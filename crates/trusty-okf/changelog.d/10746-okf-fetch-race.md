Fixed
- The document-tree import fetch opens each file once with `O_NOFOLLOW | O_NONBLOCK` and checks and reads that one handle, so a link, FIFO or larger file swapped in between the type/size check and the read is refused or never read, and opening a FIFO never blocks (#10746 S9).
