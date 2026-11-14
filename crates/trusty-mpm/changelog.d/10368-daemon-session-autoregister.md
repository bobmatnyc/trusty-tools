Fixed
- A session the daemon registers (`tm launch`, any `POST /sessions` or `mpm.sessions.register` client) or adopts on a discovery scan now registers its checkout in `projects.json`, so `tm secrets assign` finds a project with a running session. A registry failure is logged at warn and never fails the session (#10368).
