# Changelog

All notable changes are documented in this file.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).

---
## [0.13.0] — 2026-06-30

### Added

- route tm CLI through console gateway with direct fallback ([#1852](https://github.com/bobmatnyc/trusty-tools/pull/1852)) ([`b1b58bc`](https://github.com/bobmatnyc/trusty-tools/commit/b1b58bc30a423db98d21d5254c3d4d8ac71ab8a5))
- wire trusty-mpm into console reverse proxy ([#1850](https://github.com/bobmatnyc/trusty-tools/pull/1850)) ([`970d297`](https://github.com/bobmatnyc/trusty-tools/commit/970d297bf9448cf74b3117445401524bd17b20e4))
- idle auto-suspend + scrollback snapshot + resume restoration (opt-in) ([#1816](https://github.com/bobmatnyc/trusty-tools/pull/1816)) ([#1822](https://github.com/bobmatnyc/trusty-tools/pull/1822)) ([`5e28313`](https://github.com/bobmatnyc/trusty-tools/commit/5e283135fa0ea959aed720573a1302c822c02fb9))
- runtime-editable splash art + block-robot default banner ([#1825](https://github.com/bobmatnyc/trusty-tools/pull/1825)) ([#1829](https://github.com/bobmatnyc/trusty-tools/pull/1829)) ([`bd4a72a`](https://github.com/bobmatnyc/trusty-tools/commit/bd4a72a9d1dce02d27db14b894dbc725394247d5))
- auto-register git project alias on tm launch + tm ls shows local paths ([#1819](https://github.com/bobmatnyc/trusty-tools/pull/1819)) ([`436f7c9`](https://github.com/bobmatnyc/trusty-tools/commit/436f7c9194077822fb0d44de9cff4fd1f4862909))
- kawaii row-of-three pixel-bots replace scary ASCII art ([#1811](https://github.com/bobmatnyc/trusty-tools/pull/1811)) ([#1812](https://github.com/bobmatnyc/trusty-tools/pull/1812)) ([`d8e2111`](https://github.com/bobmatnyc/trusty-tools/commit/d8e2111d882c7237c77214dd122d6337d6d8fde3))
- unify daily tm banner with tm banner + hide decommissioned tombstones (#1808, #1809) ([#1810](https://github.com/bobmatnyc/trusty-tools/pull/1810)) ([`fba0160`](https://github.com/bobmatnyc/trusty-tools/commit/fba0160a2d12132ef36c2c069502b7acd9be3a5e))
- unify managed clone on shared base + per-session .worktrees/ ([#1803](https://github.com/bobmatnyc/trusty-tools/pull/1803)) ([#1804](https://github.com/bobmatnyc/trusty-tools/pull/1804)) ([`c32fc0c`](https://github.com/bobmatnyc/trusty-tools/commit/c32fc0c0623a16757c9d0a4d65d8150a29e6c2d0))
- refine tm robot banner — clip art, dedupe version, owner/repo + managed path ([#1794](https://github.com/bobmatnyc/trusty-tools/pull/1794)) ([`9183e11`](https://github.com/bobmatnyc/trusty-tools/commit/9183e11d52ee8a1076ed09c894c746af1e493ab7))
- redirect tm launch + spawn_managed_local to managed clone ([#1590](https://github.com/bobmatnyc/trusty-tools/pull/1590)) ([#1796](https://github.com/bobmatnyc/trusty-tools/pull/1796)) ([`6c0af3c`](https://github.com/bobmatnyc/trusty-tools/commit/6c0af3c798686a0eb62cdb734b0febc39d0d265c))
- detach returns to tm picker + daemon/clone cwd hardening ([#1795](https://github.com/bobmatnyc/trusty-tools/pull/1795)) ([`3b0e723`](https://github.com/bobmatnyc/trusty-tools/commit/3b0e7231e85ca8fbc53dbd55bb4968d4d96e811c))
- include repo name in managed tmux session names ([#1789](https://github.com/bobmatnyc/trusty-tools/pull/1789)) ([#1791](https://github.com/bobmatnyc/trusty-tools/pull/1791)) ([`c1887de`](https://github.com/bobmatnyc/trusty-tools/commit/c1887defa898e32ee4a424869a810b40567aa979))
- session-manager daily QoL fixes — ls --source-id, info fallback, honest decommission ([#1787](https://github.com/bobmatnyc/trusty-tools/pull/1787)) ([#1788](https://github.com/bobmatnyc/trusty-tools/pull/1788)) ([`9e9c795`](https://github.com/bobmatnyc/trusty-tools/commit/9e9c795ed07399a6e252e2071d8bc0c161dba1ff))
- add context compaction efficiency segment ([#1774](https://github.com/bobmatnyc/trusty-tools/pull/1774)) ([`0594d8f`](https://github.com/bobmatnyc/trusty-tools/commit/0594d8f10e68d485ee190f7b76f072709eb18158))
- tm guided-default auto-start daemon + github-* SSH alias support ([#1775](https://github.com/bobmatnyc/trusty-tools/pull/1775)) ([#1776](https://github.com/bobmatnyc/trusty-tools/pull/1776)) ([`20374a2`](https://github.com/bobmatnyc/trusty-tools/commit/20374a26e1b445f11dfabe6a46ce69325decd5f8))
- DOC-28 cutover catch-up runtime — watermark + git/palace + auto-inject (PR2/3/4, #1762) ([`e7e23ea`](https://github.com/bobmatnyc/trusty-tools/commit/e7e23ea2ae1a679e285391ea452272ec5bbbfee2))
- DOC-28 cutover bridge core — tm sessions catchup (PR1, #1762) ([`d66a989`](https://github.com/bobmatnyc/trusty-tools/commit/d66a989a2f6cd4e208fd1d69092d7f644da3e23e))

### Fixed

- session-worktree prune/decommission hardening ([#1845](https://github.com/bobmatnyc/trusty-tools/pull/1845)) ([#1853](https://github.com/bobmatnyc/trusty-tools/pull/1853)) ([`ff970ed`](https://github.com/bobmatnyc/trusty-tools/commit/ff970ed88549f2536f47c00bc232e70dde2561bb))
- normalise lock-file URL before TCP probe in banner ([#1847](https://github.com/bobmatnyc/trusty-tools/pull/1847)) ([#1848](https://github.com/bobmatnyc/trusty-tools/pull/1848)) ([`df5c330`](https://github.com/bobmatnyc/trusty-tools/commit/df5c330d14fd43bfd0f8174582ef52a74adf0b7c))
- CLI ergonomics fixes for tm sessions ([#1846](https://github.com/bobmatnyc/trusty-tools/pull/1846)) ([`9e4f4b9`](https://github.com/bobmatnyc/trusty-tools/commit/9e4f4b98f6e236ec6fa106f713258fba5a03ba2a))
- managed-session lifecycle correctness ([#1840](https://github.com/bobmatnyc/trusty-tools/pull/1840)) ([#1844](https://github.com/bobmatnyc/trusty-tools/pull/1844)) ([`78f2bc2`](https://github.com/bobmatnyc/trusty-tools/commit/78f2bc29a9ddb14a2bd7b9c23e1b36e740ee36f3))
- banner/health/entry UX fixes ([#1839](https://github.com/bobmatnyc/trusty-tools/pull/1839)) ([#1843](https://github.com/bobmatnyc/trusty-tools/pull/1843)) ([`1863c22`](https://github.com/bobmatnyc/trusty-tools/commit/1863c22692b85621c3e15ef30aa7436b5b0883da))
- skip worktree checkouts in auto-registration + document TRUSTY_MPM_BANNER_FILE ([#1835](https://github.com/bobmatnyc/trusty-tools/pull/1835)) ([`d2e1ab8`](https://github.com/bobmatnyc/trusty-tools/commit/d2e1ab84a8f0a37dac6e14e7d03cbc02b566b630))
- orphan-GC log-spam reduction + key-match regression tests (closes #1813) ([#1823](https://github.com/bobmatnyc/trusty-tools/pull/1823)) ([`836f393`](https://github.com/bobmatnyc/trusty-tools/commit/836f3934e005a3436d1f775480b511a23017218e))
- RAII guard kills leaked tmux sessions after test/error ([#1815](https://github.com/bobmatnyc/trusty-tools/pull/1815)) ([#1821](https://github.com/bobmatnyc/trusty-tools/pull/1821)) ([`af67403`](https://github.com/bobmatnyc/trusty-tools/commit/af67403f3c5fe016cc03c532faafc70d19e3796e))
- stop session-manager tests leaking real tmux sessions into production store ([#1790](https://github.com/bobmatnyc/trusty-tools/pull/1790)) ([#1793](https://github.com/bobmatnyc/trusty-tools/pull/1793)) ([`b3410e4`](https://github.com/bobmatnyc/trusty-tools/commit/b3410e4fa5373a7df6759a369e3ccc38d99b4a24))
- session-manager on-ramp blockers — source_id backfill + first-run clone feedback ([#1780](https://github.com/bobmatnyc/trusty-tools/pull/1780)) ([#1781](https://github.com/bobmatnyc/trusty-tools/pull/1781)) ([`313b962`](https://github.com/bobmatnyc/trusty-tools/commit/313b962c00e92b036fec76ad09d8ca72256ce367))
- non-GitHub remote refusal no longer blames daemon ([#1777](https://github.com/bobmatnyc/trusty-tools/pull/1777)) ([#1778](https://github.com/bobmatnyc/trusty-tools/pull/1778)) ([`cc9d152`](https://github.com/bobmatnyc/trusty-tools/commit/cc9d152b81991bba15c35553ec95fcfd596213a8))

### Changed

- extract DOC-28 catch-up engine behind catchup feature (PR1, #1762) ([`addfdbb`](https://github.com/bobmatnyc/trusty-tools/commit/addfdbb04ed78028887a0e782afe7cfe83c10b46))

---

## [0.12.0] — 2026-06-27

### Added

- two-panel full-width banner (robot left, info right, natural height) ([#1759](https://github.com/bobmatnyc/trusty-tools/pull/1759)) ([`37f3810`](https://github.com/bobmatnyc/trusty-tools/commit/37f3810e3dd238b4b3509c0b65d48393d355e934))
- full-screen rust robot banner + bypass-permissions launch ([#1755](https://github.com/bobmatnyc/trusty-tools/pull/1755)) ([`0924589`](https://github.com/bobmatnyc/trusty-tools/commit/092458947d3c5487e188ba260744754cfd486f37))
- ungraceful-exit handling + --resume conversation continuity (closes #1744) ([#1748](https://github.com/bobmatnyc/trusty-tools/pull/1748)) ([`40989bd`](https://github.com/bobmatnyc/trusty-tools/commit/40989bd30f2e35f9b365cdb7a877348505f9e8c1))
- expanded pre-launch welcome panel — recent commits, service status, TM commands (closes #1743) ([#1747](https://github.com/bobmatnyc/trusty-tools/pull/1747)) ([`689a9be`](https://github.com/bobmatnyc/trusty-tools/commit/689a9bed62b39640f099f038c453617a3d16d73c))
- tm welcome banner box + rich Claude Code statusline + tmux detach hint ([#1740](https://github.com/bobmatnyc/trusty-tools/pull/1740)) ([`db0a115`](https://github.com/bobmatnyc/trusty-tools/commit/db0a11553da9e81a1ee8f36b4204ee0f768f0a41))
- guided-default session picker when tm run from a repo ([#1705](https://github.com/bobmatnyc/trusty-tools/pull/1705)) ([#1729](https://github.com/bobmatnyc/trusty-tools/pull/1729)) ([`40ec125`](https://github.com/bobmatnyc/trusty-tools/commit/40ec1252d44cb29c3540b69b70bc052a935851e0))
- chat session manager MVP — force flag, turn tools, palace_dream, Task drawer (closes #1719 #1720 #1721 #1722) ([#1723](https://github.com/bobmatnyc/trusty-tools/pull/1723)) ([`7b22f28`](https://github.com/bobmatnyc/trusty-tools/commit/7b22f28e2c4f256eda0678a01fac16bd1584685b))
- in-project protected workspace + claude-mpm parity (epic #1590) ([#1715](https://github.com/bobmatnyc/trusty-tools/pull/1715)) ([`abd9914`](https://github.com/bobmatnyc/trusty-tools/commit/abd991451ba84a771ff91fc06e86e390de30ac32))
- usability sprint 1 — lock-file URL, startup prompts, TASK.md, offline swagger ([#1697](https://github.com/bobmatnyc/trusty-tools/pull/1697)) ([`d5e7e37`](https://github.com/bobmatnyc/trusty-tools/commit/d5e7e3776852d353b407d04d8623376f98298f56))
- WI-5 follow-ups — OpenRouter classifier call + auth-timeout auto-stop (closes #1648, closes #1649) ([#1656](https://github.com/bobmatnyc/trusty-tools/pull/1656)) ([`6c71d64`](https://github.com/bobmatnyc/trusty-tools/commit/6c71d646aba04e3e530b081150166518ec827dd3))
- pin palace slug in standalone MCP injection (closes #1651) ([#1655](https://github.com/bobmatnyc/trusty-tools/pull/1655)) ([`663c9ea`](https://github.com/bobmatnyc/trusty-tools/commit/663c9eab5680830157a864484f01985eaabf0dba))
- pin trusty-memory palace slug in managed-session MCP injection (closes #1605) ([#1652](https://github.com/bobmatnyc/trusty-tools/pull/1652)) ([`d15c96d`](https://github.com/bobmatnyc/trusty-tools/commit/d15c96dc846e805f2ddf6549d157d2719afd4e9a))
- SESSCTL WI-5 auth + cost model (closes #1596) ([#1647](https://github.com/bobmatnyc/trusty-tools/pull/1647)) ([`c51a5f6`](https://github.com/bobmatnyc/trusty-tools/commit/c51a5f6ae68cee071d320a92b23b168cb7c4e441))

### Fixed

- absolute-path + project-scope + opt-out for Claude hooks (fail-open hardening) ([#1756](https://github.com/bobmatnyc/trusty-tools/pull/1756)) ([`e382abb`](https://github.com/bobmatnyc/trusty-tools/commit/e382abb5c335d1b2429934dc240651cb0d608235))
- idempotent catalog sync — update existing checkout instead of failing on re-clone (closes #1751) ([#1752](https://github.com/bobmatnyc/trusty-tools/pull/1752)) ([`8a70a30`](https://github.com/bobmatnyc/trusty-tools/commit/8a70a3048bd9f699261387a213a10ce67f542a19))
- guided resume restarts a stopped session instead of raw-attaching a dead tmux session (closes #1742) ([#1745](https://github.com/bobmatnyc/trusty-tools/pull/1745)) ([`83f30ba`](https://github.com/bobmatnyc/trusty-tools/commit/83f30ba43e66baefe3715da20281a756997bc7ab))
- hermetic test isolation for managed-session & prune-idle tests (closes #1734) ([#1736](https://github.com/bobmatnyc/trusty-tools/pull/1736)) ([`d0be201`](https://github.com/bobmatnyc/trusty-tools/commit/d0be201928ab9e7c1b7e80c1d23ecb741d38536f))
- include source_id in record_to_json to match record_to_summary (closes #1733) ([#1735](https://github.com/bobmatnyc/trusty-tools/pull/1735)) ([`a901a18`](https://github.com/bobmatnyc/trusty-tools/commit/a901a18a420a31eab0a80f9d6a0c6ccaf5355e0d))
- client source_id field + daemon URL resolution probing for guided tm (closes #1730, closes #1731) ([`2f1eef5`](https://github.com/bobmatnyc/trusty-tools/commit/2f1eef59b04f104bd7444b9fbe1a11837e44cb83))
- redirect guided-default fallback to managed clone, never live checkout ([#1724](https://github.com/bobmatnyc/trusty-tools/pull/1724)) ([#1728](https://github.com/bobmatnyc/trusty-tools/pull/1728)) ([`5a7d9f1`](https://github.com/bobmatnyc/trusty-tools/commit/5a7d9f18844fee4677d0fadfe50fb0946373bd5f))

### Changed

- publish trusty-agents-common 0.1.3 + trusty-mpm 0.11.0 to crates.io ([#1750](https://github.com/bobmatnyc/trusty-tools/pull/1750)) ([`70194ec`](https://github.com/bobmatnyc/trusty-tools/commit/70194ec1788fed2e71016912dae4e062baade139))

---

## [0.11.0] — 2026-06-24

### Added

- orphan-GC PID registry + PR A nits ([#1595](https://github.com/bobmatnyc/trusty-tools/pull/1595)) ([#1637](https://github.com/bobmatnyc/trusty-tools/pull/1637)) ([`3886d33`](https://github.com/bobmatnyc/trusty-tools/commit/3886d33e077240bac3e5417427818c41a66e5d8b))
- WI-4 PR A — graceful shutdown hardening (refs #1595) ([#1617](https://github.com/bobmatnyc/trusty-tools/pull/1617)) ([`7f5ed43`](https://github.com/bobmatnyc/trusty-tools/commit/7f5ed43a646fbb1c67f2eb176703a11f696e0712))
- WI-3 SESSCTL Phase 3 — activity observability (closes #1594) ([#1600](https://github.com/bobmatnyc/trusty-tools/pull/1600)) ([`36aebaf`](https://github.com/bobmatnyc/trusty-tools/commit/36aebaf9a43d117dc7441253a52d0b648f00487e))
- WI-2 SESSCTL Phase 2 — sessctl command surface + daemon HTTP endpoints (closes #1593) ([#1599](https://github.com/bobmatnyc/trusty-tools/pull/1599)) ([`3647649`](https://github.com/bobmatnyc/trusty-tools/commit/3647649eb824ff26cdc3524ac89a1004b9e1f9f4))
- WI-1 SESSCTL Phase 1 — backend trait + SessionActor + registry foundation (closes #1592) ([#1598](https://github.com/bobmatnyc/trusty-tools/pull/1598)) ([`68d102a`](https://github.com/bobmatnyc/trusty-tools/commit/68d102a9dc8706a59120923c55bbcbca13e0dae6))
- WI-B group /fleet output by project ([#1588](https://github.com/bobmatnyc/trusty-tools/pull/1588)) ([`9d88c33`](https://github.com/bobmatnyc/trusty-tools/commit/9d88c33880f51d2f857b92f6fad7526bc4ea3c1d))
- WI-A thread repo_url/ref_ through LaunchParams + sessions.launch ([#1587](https://github.com/bobmatnyc/trusty-tools/pull/1587)) ([`64ba815`](https://github.com/bobmatnyc/trusty-tools/commit/64ba815aaf3976e87c686a4e49c8c8ff26833ccb))
- WI-1 isolation regression-guard with version capture (closes #1582, refs #1548) ([#1583](https://github.com/bobmatnyc/trusty-tools/pull/1583)) ([`23d846c`](https://github.com/bobmatnyc/trusty-tools/commit/23d846c35058128d953f71c9aaf4933e1630d67d))
- output-style filesystem deployer for managed config (closes #1553) ([#1580](https://github.com/bobmatnyc/trusty-tools/pull/1580)) ([`46e6f40`](https://github.com/bobmatnyc/trusty-tools/commit/46e6f4092606c98aed2f32459b99cbb28cc5557b))
- tm update + tm rm standalone lifecycle subcommands ([#1578](https://github.com/bobmatnyc/trusty-tools/pull/1578)) ([`b5e4b20`](https://github.com/bobmatnyc/trusty-tools/commit/b5e4b20378d122318bb8bb690911b8c51512d571))
- configurable managed-root via --root / TRUSTY_MPM_ROOT / config.toml ([#1567](https://github.com/bobmatnyc/trusty-tools/pull/1567)) ([`7f781e0`](https://github.com/bobmatnyc/trusty-tools/commit/7f781e0de2e8f9bec2863998f1c0b664c1393c37))
- WI-8 wire trusty-review MCP into tm-global managed config (refs #1548) ([#1563](https://github.com/bobmatnyc/trusty-tools/pull/1563)) ([`a4f1805`](https://github.com/bobmatnyc/trusty-tools/commit/a4f18051a8a64851b37244afda2bbb2fa002c1f0))
- WI-3 managed-session hook-clean + trust-seed + MCP-enable (refs #1548) ([#1555](https://github.com/bobmatnyc/trusty-tools/pull/1555)) ([`66ee38a`](https://github.com/bobmatnyc/trusty-tools/commit/66ee38a58d25e915d7eb98448961b45b42eca390))
- WI-2 deploy bundled agents+skills into managed CLAUDE_CONFIG_DIR (refs #1548) ([#1552](https://github.com/bobmatnyc/trusty-tools/pull/1552)) ([`bce34bd`](https://github.com/bobmatnyc/trusty-tools/commit/bce34bdec7d522d47985104bc0509ced681fbfe1))
- WI-10 managed-session auth — tm login (keychain) + ANTHROPIC_API_KEY/--bare fallback (refs #1548) ([#1551](https://github.com/bobmatnyc/trusty-tools/pull/1551)) ([`539c94a`](https://github.com/bobmatnyc/trusty-tools/commit/539c94ac89a0f38a23b0db9aee9af902d6f89690))
- MVP standalone managed driver — register/load/run with CLAUDE_CONFIG_DIR isolation (refs #1548) ([#1549](https://github.com/bobmatnyc/trusty-tools/pull/1549)) ([`81ca1b0`](https://github.com/bobmatnyc/trusty-tools/commit/81ca1b0dda406113469336f3c492933d07f3bf94))
- NL->repo resolver (WI-5, refs #1517) ([#1535](https://github.com/bobmatnyc/trusty-tools/pull/1535)) ([`222d638`](https://github.com/bobmatnyc/trusty-tools/commit/222d638e6a3f2a11b5d1939299c5a31a3b063904))
- project registry + MCP tools (closes #1519) ([#1520](https://github.com/bobmatnyc/trusty-tools/pull/1520)) ([`53f95c2`](https://github.com/bobmatnyc/trusty-tools/commit/53f95c2d61c1522adfec7e90171187d39523e578))
- wire decommission/inject verbs + graceful no-creds path in action coordinator (closes #1524) ([#1525](https://github.com/bobmatnyc/trusty-tools/pull/1525)) ([`53c49dc`](https://github.com/bobmatnyc/trusty-tools/commit/53c49dc6936c6afa8443e480c1adbc1a399f431b))
- harness-understanding instructions in trusty-agents-common + DOC-21 (closes #1510) ([#1513](https://github.com/bobmatnyc/trusty-tools/pull/1513)) ([`737cddb`](https://github.com/bobmatnyc/trusty-tools/commit/737cddbb6e8908a268604f74a41f361e13f431fc))
- track & tear down ephemeral managed sessions (closes #1508) ([#1509](https://github.com/bobmatnyc/trusty-tools/pull/1509)) ([`3b7d0c9`](https://github.com/bobmatnyc/trusty-tools/commit/3b7d0c9f1225b989687f869b7158bd83b653fc70))
- Slack adapter on the chat-core seam (#1294, epic #1433) ([#1504](https://github.com/bobmatnyc/trusty-tools/pull/1504)) ([`330a2d9`](https://github.com/bobmatnyc/trusty-tools/commit/330a2d918f597b7b91da5aec9d0a9879ec5e5aef))
- web adapter on chat-core seam (refs #1433, #1295, #926) ([#1503](https://github.com/bobmatnyc/trusty-tools/pull/1503)) ([`1c0ccdf`](https://github.com/bobmatnyc/trusty-tools/commit/1c0ccdf93269cd45482a8cb76268c35bfee3247f))
- adopt existing tmux sessions + local-path managed spawn (refs #1433) ([#1502](https://github.com/bobmatnyc/trusty-tools/pull/1502)) ([`be25fff`](https://github.com/bobmatnyc/trusty-tools/commit/be25fff3aa2effdd1f462d2b670ae84e70daf973))
- drive managed fleet from Telegram — free-text→action chat + managed slash commands ([#1501](https://github.com/bobmatnyc/trusty-tools/pull/1501)) ([`5bd5c55`](https://github.com/bobmatnyc/trusty-tools/commit/5bd5c55e985df25049967334b8d3c9cfd0828540))
- add health verb to chat-core catalog + action loop (refs #1433) ([#1498](https://github.com/bobmatnyc/trusty-tools/pull/1498)) ([`5f7b526`](https://github.com/bobmatnyc/trusty-tools/commit/5f7b526982738cc6776792037ac83f9f70be7e72))
- action-capable coordinator chat — self-aware inline verb execution ([#1496](https://github.com/bobmatnyc/trusty-tools/pull/1496)) ([`5c792e7`](https://github.com/bobmatnyc/trusty-tools/commit/5c792e7e8637bd003dbb7bfcc277f263ff4d4008))
- wire STUI slash-dispatch + free-text routing through chat-core (refs #1272, #1276) ([#1494](https://github.com/bobmatnyc/trusty-tools/pull/1494)) ([`62bb3c1`](https://github.com/bobmatnyc/trusty-tools/commit/62bb3c15cf26ee2a6fc3ae2c3d7a0d9df4899273))
- route tm CLI session verbs through chat-core; drop duplicate resolvers (refs #1283) ([#1493](https://github.com/bobmatnyc/trusty-tools/pull/1493)) ([`586f2eb`](https://github.com/bobmatnyc/trusty-tools/commit/586f2eb3032979923869441e1335e0c49a82cdd0))
- chat-core nucleus — shared command layer for session-manager adapters ([#1492](https://github.com/bobmatnyc/trusty-tools/pull/1492)) ([`baaf568`](https://github.com/bobmatnyc/trusty-tools/commit/baaf5689603be72fe7625747c76fddc488d33958))
- typed DaemonClient managed-session methods + refactor tm managed cmds ([#1491](https://github.com/bobmatnyc/trusty-tools/pull/1491)) ([`c5287af`](https://github.com/bobmatnyc/trusty-tools/commit/c5287af26a98b62376914445b1853052f2c0cd6b))
- meta run launches a real Claude Code session + verifies demo artifact (closes #1049, closes #1051) ([#1489](https://github.com/bobmatnyc/trusty-tools/pull/1489)) ([`26fbf15`](https://github.com/bobmatnyc/trusty-tools/commit/26fbf15284136569190796b390597342f9afa717))
- custom-instruction loading for the metaharness (closes #1048) ([#1485](https://github.com/bobmatnyc/trusty-tools/pull/1485)) ([`ee2c498`](https://github.com/bobmatnyc/trusty-tools/commit/ee2c498253f0ec5e9f9d9a25c5f89e9a14d35d9a))
- STUI-1 numbered scrollable session list + keybindings + state preservation (refs #1278) ([#1482](https://github.com/bobmatnyc/trusty-tools/pull/1482)) ([`5bf0009`](https://github.com/bobmatnyc/trusty-tools/commit/5bf00095aef20380f6b32aeb4604ceadbbe41e18))
- standard harness-agnostic inject_text/observe/summarize on SessionControl ([#1461](https://github.com/bobmatnyc/trusty-tools/pull/1461)) ([#1463](https://github.com/bobmatnyc/trusty-tools/pull/1463)) ([`967c892`](https://github.com/bobmatnyc/trusty-tools/commit/967c892236105928ab03b25080cd293392673299))
- periodic + startup orphan-GC reconciling registries vs tmux ls ([#1458](https://github.com/bobmatnyc/trusty-tools/pull/1458)) ([#1462](https://github.com/bobmatnyc/trusty-tools/pull/1462)) ([`aa1c8f8`](https://github.com/bobmatnyc/trusty-tools/commit/aa1c8f8cee680648f8cd5231778f5bbb87a5308d))
- owning tmux Session guard with RAII Drop reaper + test teardown guards (refs #1453, #1459, epic #1452) ([#1460](https://github.com/bobmatnyc/trusty-tools/pull/1460)) ([`ff25808`](https://github.com/bobmatnyc/trusty-tools/commit/ff25808cf8fcb682851d6ba376501cece5674e6f))
- sessions TUI startup banner + service probes (STUI-0) ([#1431](https://github.com/bobmatnyc/trusty-tools/pull/1431)) ([`734b5e5`](https://github.com/bobmatnyc/trusty-tools/commit/734b5e54f2688052cadb2f5e3c26f8ab2d09b139))
- coordinator-context last_summary + summarizing flag (STUI-4) ([#1432](https://github.com/bobmatnyc/trusty-tools/pull/1432)) ([`0fda534`](https://github.com/bobmatnyc/trusty-tools/commit/0fda534e6359d8963ad59044421d4a6a82822afd))
- catalog update-check + rebuild/apply (closes #1408) ([#1429](https://github.com/bobmatnyc/trusty-tools/pull/1429)) ([`41b312a`](https://github.com/bobmatnyc/trusty-tools/commit/41b312a5cb78c469e9c3b0968107c2fd90340203))
- manifest-driven harness provisioning (HR-2, #1407) ([#1427](https://github.com/bobmatnyc/trusty-tools/pull/1427)) ([`f012658`](https://github.com/bobmatnyc/trusty-tools/commit/f012658d26560735b2659a4649cabff41b4ebac8))
- multi-style output + version-fallback injection (HR-4) ([#1412](https://github.com/bobmatnyc/trusty-tools/pull/1412)) ([`77ad339`](https://github.com/bobmatnyc/trusty-tools/commit/77ad33964556158f9816cf9e0fd7de7967ee9114))
- BASE agent content parity + initialPrompt/tier-model injection ([#1411](https://github.com/bobmatnyc/trusty-tools/pull/1411)) ([`0a28b24`](https://github.com/bobmatnyc/trusty-tools/commit/0a28b24550e041139b59c79daead1da3671d1f29))
- wire in-process AgentRunner into meta run orchestrator (closes #1030) ([#1396](https://github.com/bobmatnyc/trusty-tools/pull/1396)) ([`19b972d`](https://github.com/bobmatnyc/trusty-tools/commit/19b972d6e6158ebbc0013a59561c724226d2213a))
- coordinator TUI live session-list polling (Child #2, refs #1274) ([#1386](https://github.com/bobmatnyc/trusty-tools/pull/1386)) ([`44345df`](https://github.com/bobmatnyc/trusty-tools/commit/44345df721f9af648b0b9ac5bdc22d2ee1bcc5dc))
- coordinator TUI skeleton screen + tm coordinator-tui subcommand (Child #1, refs #1272) ([#1383](https://github.com/bobmatnyc/trusty-tools/pull/1383)) ([`2dab8d4`](https://github.com/bobmatnyc/trusty-tools/commit/2dab8d4588a139b7179716f25d8e657548cc5a72))
- wire trusty-code ToolRegistry into tm meta run (WI-2, refs #1045) ([#1384](https://github.com/bobmatnyc/trusty-tools/pull/1384)) ([`0684a7d`](https://github.com/bobmatnyc/trusty-tools/commit/0684a7d2477716904077a7381745f220b5e5c1ed))
- bootstrap tm meta run subcommand (WI-1, refs #1045) ([#1382](https://github.com/bobmatnyc/trusty-tools/pull/1382)) ([`cdaa6c7`](https://github.com/bobmatnyc/trusty-tools/commit/cdaa6c728a7d491984c32fbd27c33e64163b92c9))

### Fixed

- repair daemon::state overseer tests and daemon::api blocking-client panic (closes #1571, closes #1523) ([#1581](https://github.com/bobmatnyc/trusty-tools/pull/1581)) ([`216ee11`](https://github.com/bobmatnyc/trusty-tools/commit/216ee1144ff3173ed5fb59b37383cef10daa30c7))
- managed MVP polish — atomic-save cleanup, credential direction, idempotent .mcp.json ([#1579](https://github.com/bobmatnyc/trusty-tools/pull/1579)) ([`e1721ad`](https://github.com/bobmatnyc/trusty-tools/commit/e1721ad6943f6a32a52a7839aad16c01634cd1de))
- atomic confirm_pair_code with crash-safe claim cleanup (closes #1506) ([#1547](https://github.com/bobmatnyc/trusty-tools/pull/1547)) ([`731d915`](https://github.com/bobmatnyc/trusty-tools/commit/731d91511327124eb795fea5c6ffdfc31db18427))
- stop dropping tokio Runtime in async test context (closes #1521) ([#1522](https://github.com/bobmatnyc/trusty-tools/pull/1522)) ([`ac9365b`](https://github.com/bobmatnyc/trusty-tools/commit/ac9365b3a511ea72fd796ccb6c4e6aafb5dbd25c))
- HTML-escape Telegram command replies (closes #1514) ([#1515](https://github.com/bobmatnyc/trusty-tools/pull/1515)) ([`0e577e3`](https://github.com/bobmatnyc/trusty-tools/commit/0e577e3ab7ff7424e5a5a6242c4094e017170309))
- guard decommission against deleting non-owned workspaces (P0, closes #1511) ([#1512](https://github.com/bobmatnyc/trusty-tools/pull/1512)) ([`435a962`](https://github.com/bobmatnyc/trusty-tools/commit/435a962fc5ae631d5d11afdfc3c771fb9c2d653b))
- supervise Telegram bot + unify pairing-code store (closes #1499, closes #1500) ([#1505](https://github.com/bobmatnyc/trusty-tools/pull/1505)) ([`b5507a3`](https://github.com/bobmatnyc/trusty-tools/commit/b5507a3e59849f56e174af07c415ec448b4a7ee7))
- make telegram bot username configurable via TELEGRAM_BOT_USERNAME (default t_sess_bot) (refs #1433) ([#1497](https://github.com/bobmatnyc/trusty-tools/pull/1497)) ([`897df90`](https://github.com/bobmatnyc/trusty-tools/commit/897df90bdc25156aa8d190e7a4fdb0b5cd1a83c6))
- tmux lifecycle rollbacks — spawn send_line + registry upsert (#1456, #1457) ([#1468](https://github.com/bobmatnyc/trusty-tools/pull/1468)) ([`e471ee2`](https://github.com/bobmatnyc/trusty-tools/commit/e471ee2ead502b013f41ea59e40e75c13475ba45))
- tmux lifecycle — DELETE kills session + graceful-shutdown reaper (#1454, #1455) ([#1466](https://github.com/bobmatnyc/trusty-tools/pull/1466)) ([`4c53699`](https://github.com/bobmatnyc/trusty-tools/commit/4c5369923e592288e9b0e5d41dec028ec345078b))

### Changed

- WI-2 review nits — single missing-source hint + document deploy layout (refs #1548) ([#1556](https://github.com/bobmatnyc/trusty-tools/pull/1556)) ([`404b874`](https://github.com/bobmatnyc/trusty-tools/commit/404b8744e72ffeec07a2f1e12cb3eeeb217b38b1))
- rename CLI 'session' command group to 'sessions' (closes #1394) ([#1395](https://github.com/bobmatnyc/trusty-tools/pull/1395)) ([`864b006`](https://github.com/bobmatnyc/trusty-tools/commit/864b0062bfd0a3d913855c45fa9d246bca13634f))
- rename `tm coordinator-tui` → `tm session tui` + move coordinator API under /api/v1/sessions ([#1393](https://github.com/bobmatnyc/trusty-tools/pull/1393)) ([`749e7dd`](https://github.com/bobmatnyc/trusty-tools/commit/749e7dd4bad08ff8f93c04bb7c3d36991221f79b))

---

## [0.10.0] — 2026-06-17

### Fixed (closes #1373)

- **Sessions now register + pin their own project's trusty-search index.** At
  session launch `prepare_session` derives the project's canonical index id
  (git-root basename, via the shared `trusty_common::derive_index_id`),
  best-effort find-or-creates it in the running trusty-search daemon
  (`POST /indexes`), and injects the `trusty-search` MCP stub **pinned** to that
  id (`serve --index <id>`). A bare `search`/`grep` therefore resolves to the
  session's own project index instead of letting the LLM guess — which
  routinely picked the wrong (usually persistent `claude-mpm`) index. The
  daemon-unreachable case is graceful: it logs a warning and still pins the
  stub (the index is created on first reindex); an empty derived id falls back
  to the unpinned `serve` stub. Either way the session always launches.

## [0.9.0] — 2026-06-16

### Release

- **First monorepo publish.** This is the first `trusty-mpm` release published
  from the unified `trusty-tools` workspace. It supersedes the stale `0.8.1`
  on crates.io, which was published from the now-archived standalone repo.

### Fixed

- **Standalone build break:** `daemon/mcp_console.rs` imports
  `trusty_common::console_metrics` unconditionally, but that module is gated
  behind trusty-common's `console-metrics` feature. trusty-mpm's main
  `trusty-common` dependency now enables `console-metrics`, so
  `cargo check -p trusty-mpm` and `cargo publish` no longer fail to resolve
  the module. (Workspace feature-unification previously masked this under
  `cargo test`.)

## [0.8.2] — 2026-06-16

### Changed (closes part of #1318)

- **De-bundled `trusty-console`.** Removed the bundled `trusty-console`
  `[[bin]]` shim and dependency. `cargo install trusty-mpm` now produces
  `tm` and `trusty-mpm` only. Install the console with
  `cargo install trusty-console`. This is part of the single-owner-per-binary
  fix for the cargo binary-ownership collisions (#1262).

## [Unreleased]

### Changed: CLI command group `session` → `sessions` (issue #1394)

The top-level CLI command group was renamed from the singular `session` to the
plural **`sessions`** to match the `/api/v1/sessions/*` HTTP API surface. Every
subcommand is now invoked under the plural name, e.g. `tm sessions tui`,
`tm sessions ls`, `tm sessions new`.

- The singular `session` spelling is **removed entirely** — it is not retained
  as an alias. Invoking `tm session …` now fails with an
  unrecognized-subcommand error. Update any scripts or muscle memory to
  `tm sessions …`.
- This is a CLI-only change; the HTTP API (already `/api/v1/sessions/*`) and the
  separate `session-manager` / `sm` coordinator command are unaffected.

### Deprecated: verbose managed session-lifecycle verbs (issue #1205)

The managed session-lifecycle CLI verbs were renamed to the cleaner, symmetric
`stop` / `resume` / `decommission` family. The old verbose verbs still work but
now emit a one-line deprecation notice to **stderr** on every invocation and
will be removed in a future release.

| Deprecated verb | Use instead | Behavior |
|-----------------|-------------|----------|
| `tm sessions runtime-stop <id>` | `tm sessions stop <id>` | Stop the runtime, keep the workspace (resumable) |
| `tm sessions managed-stop <id>` | `tm sessions stop <id>` | Same as `runtime-stop` |
| `tm sessions managed-resume <id>` | `tm sessions resume <id>` | Re-spawn the runtime in the existing workspace |

- The deprecated verbs are hidden from `tm sessions --help` but continue to parse
  for backward compatibility.
- Each deprecated invocation prints `warning: '<old>' is deprecated; use '<new>'`
  to stderr; stdout stays clean for scripts.
- `tm sessions decommission <id>` (terminal teardown: remove workspace from disk)
  is unchanged.

## [0.5.0] — 2026-05-28

### Added: `tm services` — canonical service-discovery CLI (issue #339)

**New subcommand**: `tm services <action>` — replaces ad-hoc `lsof`/`curl`/`ps`
patterns for discovering the port, health, and status of every trusty-* daemon.

#### Subcommands

| Command | Description |
|---------|-------------|
| `tm services list [--json]` | Table of all declared services with running/down status, port, version, and health |
| `tm services status <name> [--json]` | Detailed block for one service |
| `tm services port <name>` | Print just the port number (scriptable) |
| `tm services url <name>` | Print the full base URL |
| `tm services health <name>` | Probe the `/health` endpoint; exit 0 if healthy |
| `tm services log <name>` | Print the log file path if it exists |
| `tm services init [--force]` | Write the default manifest to `~/.claude-mpm/services.yaml` |
| `tm services restart <name>` | Execute the manifest `restart_cmd` |

#### Manifest

Default manifest embedded in the binary covers 6 services:

- `trusty-search` — port 7878, `/health` confirmed
- `trusty-analyze` — port 7879, `/health` confirmed
- `trusty-mpm-daemon` — port 7880, `/health` confirmed at `daemon/api.rs:74`
- `trusty-memory` — dynamic port (7070-7079) via `~/.trusty-memory/http_addr`
- `trusty-embedderd` — UDS sidecar, pgrep-only (no HTTP surface)
- `trusty-bm25-daemon` — UDS sidecar, pgrep-only (no HTTP surface)

Custom manifests can be placed at `~/.claude-mpm/services.yaml` (use `tm services init`).

#### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Running/healthy (list always exits 0) |
| 1 | Service declared but down, or health probe failed |
| 2 | Service name not in manifest |

#### Scriptable usage

```bash
PORT=$(tm services port trusty-search)
URL=$(tm services url trusty-search)
tail -f $(tm services log trusty-search)
```

#### Architecture

- `crates/trusty-mpm/src/services/manifest.rs` — `ServicesManifest`, `ServiceDecl`,
  `PortDiscovery` enum, `ManifestValidationError` (thiserror)
- `crates/trusty-mpm/src/services/discoverer.rs` — `Discoverer` with 5-second
  TTL cache; `ProcessProber`/`PortProber`/`HttpProber`/`VersionRunner` trait
  seams for unit testing
- `crates/trusty-mpm/assets/default-services.yaml` — embedded default manifest

**Tests**: 21 new unit tests (8 manifest + 13 discoverer, all mocked) + 11 CLI
parse tests + 2 ignore-gated integration smoke tests.

---

## [consolidation] — 2026-05-26

**Combined 7 trusty-mpm-\* sub-crates into one crate with feature-gated `[[bin]]` targets.**

### Summary

The following sub-crates have been merged into this unified `trusty-mpm` crate:

| Former crate | Now lives in |
|---|---|
| `trusty-mpm-core` | `crates/trusty-mpm/src/core/` |
| `trusty-mpm-client` | `crates/trusty-mpm/src/client/` |
| `trusty-mpm-mcp` | `crates/trusty-mpm/src/mcp/` (feature: `mcp`) |
| `trusty-mpm-daemon` | `crates/trusty-mpm/src/daemon/` (feature: `daemon`) |
| `trusty-mpm-cli` | `crates/trusty-mpm/src/bin/tm.rs` (feature: `cli`) |
| `trusty-mpm-tui` | `crates/trusty-mpm/src/tui/` (feature: `tui`) |
| `trusty-mpm-telegram` | `crates/trusty-mpm/src/telegram/` (feature: `telegram`) |

The Tauri desktop GUI (`trusty-mpm-gui`) remains as a separate crate because
it owns `build.rs` (invoking `tauri_build::build()`) and `tauri.conf.json` — files
that cannot co-exist with a generic Cargo crate build system. The `gui` feature of
this crate wraps it as an optional path dependency.

### Workspace crate count
- Removed: 7 crates (`trusty-mpm-core`, `trusty-mpm-mcp`, `trusty-mpm-daemon`,
  `trusty-mpm-client`, `trusty-mpm-cli`, `trusty-mpm-tui`, `trusty-mpm-telegram`)
- Added: 1 crate (`trusty-mpm`)
- Net change: 28 → 22 workspace members

### Feature flags

| Feature | What it enables |
|---|---|
| `default` | `cli` + `daemon` (the common install path) |
| `cli` | `tm` / `trusty-mpm` CLI binary (implies `daemon`, `tui`, `telegram`) |
| `daemon` | `trusty-mpmd` daemon binary + daemon library module (implies `mcp`) |
| `mcp` | MCP server library module |
| `tui` | `trusty-mpm-tui` shim binary + TUI library module |
| `telegram` | `trusty-mpm-telegram` shim binary + Telegram library module |
| `gui` | `trusty-mpm-gui` shim binary (wraps the separate `trusty-mpm-gui` crate) |

### Public API surface

All public types, traits, and functions are preserved. The only change is the
import path: code that previously imported from `trusty_mpm_core`, `trusty_mpm_client`,
etc. should now import from the corresponding submodule of `trusty_mpm`:

```rust
// Before
use trusty_mpm_core::session::{Session, SessionId};
use trusty_mpm_client::DaemonClient;

// After
use trusty_mpm::core::session::{Session, SessionId};
use trusty_mpm::client::DaemonClient;
```

### Deprecation notes

The following crate names are no longer published:
- `trusty-mpm-core`
- `trusty-mpm-mcp`
- `trusty-mpm-daemon`
- `trusty-mpm-client`
- `trusty-mpm-cli`
- `trusty-mpm-tui`
- `trusty-mpm-telegram`

All functionality is available under `trusty-mpm` with the appropriate feature flags.

## [0.4.0] and prior

See the individual crate changelogs in the former sub-crate directories (available
in git history as `crates/trusty-mpm-{core,client,mcp,daemon,cli,tui,telegram}/`).
