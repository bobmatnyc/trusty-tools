Fixed

- M005 re-reads each file through `core::extract::read_content`, the same seam the ingest and watch paths use, so `.docx`, `.pdf` and spreadsheet files are extracted rather than read as raw UTF-8. Reading them as text produced no chunks, and the orphan sweep then dropped every vector they had — matsuoka-com fell from 64,517 chunks to 3,526 on its first query after upgrading (#6910).
- A file that is still on disk but cannot be extracted keeps its vectors instead of being swept as an orphan. Removal now needs affirmative evidence that the file is gone from disk; an existence probe that errors keeps the vectors (#6910).
