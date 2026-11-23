Security
- `computer_read` and `computer_verify` no longer return a screenshot. cua-driver attaches an unredacted window image to `get_window_state` by default; the broker now sends `include_screenshot: false` for the read tools and drops any image part from every tool reply.
