Fixed
- The document-tree import fetch opens each file once with `O_NOFOLLOW | O_NONBLOCK` and checks and reads that one handle, so a link, FIFO or larger file swapped in between the type/size check and the read is refused or never read, and opening a FIFO never blocks (#10746 S9).
- The fetch opens every directory relative to the previous directory handle with `O_NOFOLLOW | O_DIRECTORY`, so a parent directory swapped for a symbolic link cannot lead outside the source root (#10746 S9).
