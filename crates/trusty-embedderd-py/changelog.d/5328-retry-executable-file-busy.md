Fixed
- A venv recheck no longer reports `Failed` when Linux refuses to exec the interpreter with ETXTBSY ("Text file busy") because another process briefly holds it open for writing; the spawn retries within its budget like EAGAIN/ENOMEM (#5328).
