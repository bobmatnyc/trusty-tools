Fixed
- `session_send` and `tm sessions send` re-read the pane for up to 3 s after Enter while a collapsed paste still shows, so a paste a busy receiver commits a second later reports `submitted` instead of `unsubmitted_paste`. A busy receiver that still shows the paste after the window reports `unverified` with a reason that says not to resend; it never reports `sent: false` (#11798).
