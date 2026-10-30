Fixed
- The `index_file` reply now counts the chunks committed after the window split (the windows), not the chunks the parser produced (#10504). A long function stored as 9 windows used to answer `chunks: 1`.
