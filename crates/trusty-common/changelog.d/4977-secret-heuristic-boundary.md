Fixed
- Memory secret scanner: a `/`-bearing base64 blob is no longer exempted as a slash path. Every `/`-separated run of standard base64 is pure alphanumeric, so the charset-only segment test called it a path and let credentials — including a canonical AWS secret access key — store unredacted (#4977).
- Memory secret scanner: a bare, userinfo-free URL such as a GitHub issue or PR link no longer fails a write. Its path digits used to satisfy the base64 branch's entropy floor; the URL is now decomposed and decided segment by segment, so a URL whose path IS the secret stays blocked (#5513).
