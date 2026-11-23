Security
- `computer_read` and `computer_verify` no longer return an image: cua-driver 0.23.2 attaches an unredacted screenshot by default, so the broker now sends `include_screenshot: false` and drops any image part from the reply (#11694).
