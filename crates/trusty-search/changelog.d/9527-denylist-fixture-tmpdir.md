Fixed
- The #767 denylist tests build their denylisted fixture explicitly under `/tmp`, so they no longer fail on a host whose `TMPDIR` is outside the sensitive-path denylist (EVO, `/var/tmp`). The denylist itself is unchanged. (#9527)
