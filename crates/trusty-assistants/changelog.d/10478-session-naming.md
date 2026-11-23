Fixed
- `next_session_name` gives bare `tm-<project>-<harness>` for the first session and strips a project's own `tm-` prefix, through the shared trusty-common rule; a duplicate gets `-02` onward (#10478).
