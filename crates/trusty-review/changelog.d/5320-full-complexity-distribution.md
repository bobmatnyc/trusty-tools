Fixed

- The §7 complexity distribution is fetched from trusty-analyze's new full-corpus histogram instead of being bucketed from a truncated top-1000 hotspot list, so its bands and percentages describe the whole codebase. On a daemon that serves no histogram the table is omitted and the reason stated under Gaps & Caveats, rather than rendering shares of a truncation as if they were shares of the codebase (#5320).
