Changed
- The project picker reads trusty-mpm's project registry with `mpm.projects.registry.list` on the daemon's Unix socket (`TRUSTY_MPM_SOCKET` pins it), not `GET http://127.0.0.1:7880/api/v1/projects`; `TRUSTY_MPM_URL` no longer applies. An absent socket or an error answer degrades to the filesystem-only roster (`source: fs_only`) as before (#6288).
