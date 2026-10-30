Fixed
- The `umask_000_end_to_end_export_is_owner_only_and_leaks_nothing` test no longer flakes: its 4-char leak scan now ignores the random characters of temp-dir names, which could match a token window by chance (#10549).
