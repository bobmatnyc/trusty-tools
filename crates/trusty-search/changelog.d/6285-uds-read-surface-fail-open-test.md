Fixed

- **A daemon that cannot bind its socket is now proven to exit rather than serve HTTP alone** (refs [#6285](https://github.com/bobmatnyc/trusty-tools/issues/6285)). The behaviour shipped in slice 1; what was missing was a test driving `run_daemon()` itself. `run_daemon_refuses_a_socket_another_process_is_serving` starts a real listener on the isolated socket path first, then calls `run_daemon`, and asserts both that it returns an error naming the path and that no `http_addr` file was written — so a half-bound daemon can never announce itself to the consumers the retire slice will move.
