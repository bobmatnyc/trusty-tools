# tm tui: console look and Architect view

Status: draft for owner review. Goal `ar-G01` (Bob, 2026-11-10). Scope: design only.
Foundry (`README.md` here) is the web design system; this note maps its Night Shift dark
palette and the console Overview layout onto a ratatui screen. No new design tokens.

Constraints (owner rulings, 2026-11-09): `tm tui` uses UDS only, no native HTTP (#10899).
Data comes from existing daemon methods and the console UDS listener (#10902). Keep it simple.

## 1. Where `tm tui` stands (read at `525f15cf80`)

- `crates/trusty-mpm/src/tui/home/` is the opening screen: a 4-column tile grid
  (`home/render.rs::render_grid`), one tile per service, a service view on Enter.
  Tile status comes from UDS `<svc>.health` probes (`home/services.rs::probe`). Agents and
  Console are still `ProbeKind::HttpAddr` (open: #10899).
- Per-service metrics are a stdio spawn of `console_metrics` (`home/probe.rs`), cached 30 s.
  There is no history, so no sparklines.
- `tui/project_ctl/` is the projects view (Enter on the mpm tile). Since #10754 it is two
  columns: Owner -> Project tree left (30%), the node's sessions right (`project_ctl/layout.rs::main_columns`),
  with an Activity strip and an action bar. It reads the mpm daemon through `DaemonClient`,
  which has a unix-socket mode (`socket: Option<PathBuf>`, #6288).
- Colors are bare ANSI names (`Color::Green`, `Color::DarkGray`) in about 10 files. There is no
  theme module.
- ratatui is 0.30 (workspace `Cargo.toml`), feature `tui`. Its widget crate ships `Sparkline`,
  `Chart`, `BarChart`, `Gauge`, `Table`, `Tabs`, `Block`, `Scrollbar`, `Canvas`.

## 2. Where the console gets its data today

| Console element | Source (HTTP, browser) | UDS-reachable? |
|---|---|---|
| Machine cards: CPU, memory, swap, disk, network, pressure badge | `GET /api/console/machine-status` | No (gap G1) |
| Card sparklines (1 Hz ring, 600 samples) | `GET /api/console/machine-status/history`, SSE `/stream` (`machine_history::HistorySnapshot.samples`) | No (G1) |
| Services roster, version, status badge | `GET /api/console/services` | Yes after #10902 `console.fleet_status` |
| Per-service CPU/MEM last 10 min | same snapshot, `service_samples` (`{id, status, cpu_pct, rss_bytes}`) | No (G2) |
| Per-service detail metrics | stdio `console_metrics` | Yes after #10906 / #10907 (`<svc>.console_metrics`, `by_*`) |
| Architect link | `GET /api/console/architect-dashboard` | Not needed; see section 4 |

The history rings live only in the console process (`crates/trusty-console/src/machine_history/`).
#10902 lists `console.health` and `console.fleet_status` and nothing else; #10907 adds
`console.console_metrics`, which counts console requests and webhooks, not host load.
Neither returns host metrics or any history.

## 3. Home view: the console look

Sections, top to bottom, mirroring `ConsoleTab.svelte`: header, MACHINE STATUS, SERVICES.

```
 TRUSTY CONSOLE   UNIT-05 · SERVICE CONSOLE · tm 1.12.1            [a]rchitect  [?]
 MACHINE STATUS                                                         NOMINAL
 ┌ CPU ─────── OK ┐┌ MEMORY ──── OK ┐┌ DISK ────── OK ┐┌ NETWORK ───── OK ┐
 │ 18.6%          ││ 57.9 / 128 GiB ││ 5815 / 7444 GiB││ v 1.2  ^ 0.3 MB/s│
 │ 16 logical     ││ swap 2.6/4.0   ││ 78.1% used     ││ over 1.0 s       │
 │ ▁▂▂▃▂▂▄▃▂▂▃▂▂▃ ││ ▅▅▅▅▅▆▆▆▆▆▆▆▆▆ ││ ▇▇▇▇▇▇▇▇▇▇▇▇▇▇ ││ ▁▁▃▁▁▁▅▁▁▂▁▁▁▃▁   │
 └────────────────┘└────────────────┘└────────────────┘└──────────────────┘
 SERVICES
  Service   Version  Status      %CPU  MEM       CPU · last 10 min  MEM · last 10 min
  Agents    0.31.0   AVAILABLE    0.4  212 MiB   ▁▁▂▁▁▁▃▁▁▁▂▁       ▃▃▃▃▃▃▃▃▃▃▃▃
  Console   0.12.6   RUNNING      1.1   96 MiB   ▂▂▁▂▂▃▂▂▁▂▂▂       ▂▂▂▂▂▂▂▂▂▂▂▂
  Memory    0.9.2    READY        0.2  310 MiB   ▁▁▁▁▂▁▁▁▁▁▁▁       ▄▄▄▄▄▅▅▅▅▅▅▅
  MPM       1.12.1   RUNNING      2.3  140 MiB   ▂▃▂▂▅▃▂▂▃▂▂▃       ▃▃▃▃▃▃▃▃▃▃▃▃
 [↑↓] select  [Enter] open  [a] architect  [q] quit
```

- Cards: `Block` with a border-strong title, a bold value line, a muted meta line, and a
  `Sparkline` filling the bottom row. Badge text is colored by `Pressure` (nominal / elevated /
  critical), the same mapping as `machineStatus.js::pressureTone`. A null sample draws a gap,
  not zero (console rule).
- Services: a ratatui `Table`. The two sparkline columns are one `Sparkline` per row, drawn
  into the cell `Rect` after the table renders (a `Table` cell holds text, not widgets).
  Width of a sparkline cell is the sample count shown; at under 100 columns the sparkline columns
  drop and the row keeps value text.
- Enter on a row keeps today's behavior: mpm opens the projects view, other services open their
  service view. The tile grid goes away; `HomeState.tiles` becomes the table rows.
- Below 80 columns: cards stack 2x2, the services table drops the version column.

## 4. Architect view

Opened with `a` from Home. Tabs across the top mirror the dashboard categories, with open
counts: Decisions, Blockers, Goals, Sessions, Tasks, Q&A, and **Live** in place of Visualize.
Lists are `Table`/`List` with an id column, a title and a status badge; Enter expands a row
into a detail pane below; Esc returns.

```
 ARCHITECT   Decisions 3 · Blockers 1 · Goals 7 · Sessions 12 · Tasks 9 · Q&A 2 · [Live]
 ┌ Live ─────────────────────────────────────────────────────────────────────────┐
 │ ● tm-03            goal G01   working     ctx 41%   waiting on: -            │
 │ │  ├ engineer      tm tui theme module            running 6m                 │
 │ │  └ qa            cargo test -p trusty-mpm       running 1m                 │
 │ ◐ ar-pm-02         goal G04   waiting-ci  ctx 22%   PR #10931                │
 │ │  └ (no agents)   idle · last relay 4m ago                                  │
 │ ○ ts-01            goal G02   idle        ctx 63%                            │
 │ │  └ (no agents)                                                             │
 └───────────────────────────────────────────────────────────────────────────────┘
 Decisions 3 open · Blockers 1 · Tasks 9 open · Q&A 2 waiting          [Tab] next tab
```

The Live tab is a vertical tracker: one row per session (state glyph, name, goal, state word,
context %, waiting-on), the session's running agents indented below it as a tree, each with the
task text and elapsed time. It redraws on the poll tick; a collapsed session shows only its
row. This is a `List` of styled `Line`s, no graph widget. An optional bottom `BarChart` of
agents per session is left out (section 8, Q4).

## 5. Data source per panel

| Panel | UDS method | Status |
|---|---|---|
| Header, version, Console badge | `console.health` | Pending #10902 |
| Services table: name, version, status | `console.fleet_status`; fallback per-service `<svc>.health` probes (exist today) | Pending #10902 |
| Services %CPU / MEM now | `<svc>.console_metrics` for service-specific values | Pending #10906 / #10907; **host-process CPU/RSS is not in any of them (G2)** |
| Machine cards (CPU, mem, swap, disk, net, pressure) | none | **G1** |
| Card and service sparklines (10 min) | none | **G1, G2** |
| Architect Decisions / Blockers / Goals / Sessions / Tasks / Q&A | `mpm.architect.dashboard.list` (`collection`, `status`, `order_by`), `.tally`, `.footer` | Exists (`trusty-architect` `dashboard::rpc::METHODS`); needs the store root (G3) |
| Live: sessions | `mpm.sessions.list` or `mpm.managed.fleet` | Exists |
| Live: agents per session | `mpm.delegation.list` (`Delegation { session, agent, task, status, created_at }`) | Exists; confirm the TUI-callable shape in slice 5 |
| Live: current step per session | `mpm.managed.activity` (`state`, `summary`) | Exists; polled for the selected session only today |
| Events / bus health pill | `events.health` | Held in #10899; drop the pill until it lands |

Gaps to file (one issue each, search first per `docs/reference/issue-search-keys.md`):

- **G1.** `console.machine_status` and `console.machine_history` on the #10902 listener,
  returning what `routes/machine_status.rs` and `routes/machine_history.rs` serve today
  (`HostMetrics` plus the `samples` ring). Needs an owner ruling like D8b, because #10902
  scopes the listener to `health` and `fleet_status`.
- **G2.** The same history call must include `service_samples`, or `console.fleet_status` must
  carry the latest sample per service. Without it the per-service sparklines have no source.
- **G3.** The Architect store root. The dashboard methods take `store {root, format}` on every
  call; nothing tells `tm tui` which one to use. Needs either a daemon-side default
  (`mpm.architect.dashboard.*` resolves the instance store) or a `tm` config key.
- **G4 (inference, not confirmed).** The Architect dashboard's session `details.current` and
  `usage.agents_active` are written by the Architect poller into the store; the Live tab can
  take them from the `sessions` collection if `mpm.delegation.list` is too coarse for a
  "current step". Decide after reading live output in slice 5.

If the owner does not want G1/G2 on console, the fallback is for `tm tui` to sample the host
itself with `sysinfo` (already in `Cargo.lock`) and keep its own 10-minute ring. That duplicates
the console sampler and diverges from "data through trusty-console", so it is not the proposal.

## 6. Crate choices

- **ratatui 0.30 built-ins: `Sparkline`, `Table`, `Block`, `Tabs`, `Gauge`, `BarChart`,
  `Chart`.** Already a dependency. `Sparkline` covers every console graph (a bar series with
  eight height levels). `Chart` is reserved for a larger detail view (host CPU line over 10 min,
  opened from a card). `BarChart` is optional (Q4). No new dependency.
- **No `tui-big-text`.** The "big value" is a bold line, 1 row, in the accent color. A 3-row
  font costs a crate and card height for no information.
- **No `sysinfo` in the TUI.** Host numbers come from console (G1).
- **Own `theme` module, no theme crate.** About 40 lines: a `Theme` struct of named styles.

## 7. Palette mapping

Source: Foundry dark tokens in `crates/trusty-console/ui/src/theme.css` (`[data-theme="dark"]`).
New file `tui/theme.rs` owns all colors; panes ask for roles (`theme.badge_ok`), never a `Color`.

| Role | Foundry token | Truecolor | 256-color | 16-color |
|---|---|---|---|---|
| background | `--trusty-content-bg` | `#201612` | 234 | Black (terminal default) |
| card / panel | `--trusty-card-bg` | `#2b1c12` | 235 | Black |
| border | `--trusty-border` | `#46311f` | 237 | DarkGray |
| text | `--trusty-text-primary` | `#f0e7d8` | 255 | White |
| text muted | `--trusty-text-muted` | `#a58a6b` | 138 | DarkGray |
| accent, sparklines | `--trusty-accent` | `#d97742` | 173 | Yellow (amber) |
| ok / running / ready | `--trusty-success` | `#8fbf6a` | 107 | Green |
| warn / available | `--trusty-warning` | `#d9a83a` | 178 | Yellow |
| degraded | `--trusty-status-degraded` | `#e2924f` | 173 | Yellow + bold |
| danger / down | `--trusty-danger` | `#e06a52` | 167 | Red |

Detection, once at startup: `COLORTERM` of `truecolor` or `24bit` gives RGB; else `TERM`
containing `256color` gives indexed; else 16-color. `NO_COLOR` set gives no color, with badges
distinguished by their text and a `Modifier` (bold, reverse). In 16-color mode accent and warn
share Yellow, so badge text (`AVAILABLE`, `DEGRADED`) carries the meaning, never color alone.
The background is painted only in truecolor and 256-color; in 16-color the terminal background
stays, to avoid a black box on a light terminal. Light theme is out of scope (Q5).

## 8. Slice plan

One PR per slice, in order. Builds and tests run on EVO while the Mac Rust band is stopped.
Every slice's done check: (a) a screenshot or asciinema of `tm tui` placed next to the console
at the same terminal width, attached to the PR, and (b) a live check on Studio against the
installed daemons. Ladder rung 3 for rendering slices (one crate), rung 5 where a method
contract is touched.

1. **Theme module + badges.** `tui/theme.rs`, color-depth detection, `badge()` helper; the
   home tiles and header adopt it. No data change. Done: palette visible on Studio in truecolor
   and with `TERM=xterm` (16-color); unit tests for the three depths and `NO_COLOR`.
2. **Services table.** Replace the tile grid with the services `Table` and badges. Data:
   `console.fleet_status` once #10902 lands, else the existing per-service probes. No
   sparklines yet. Done: row set and statuses match the console Services list on Studio.
3. **Machine status cards.** Four cards with value, meta, badge. Blocked on G1. Done: values
   match the console cards within one sample.
4. **Sparklines.** Card sparklines and the two per-service columns, 10-minute window. Blocked on
   G1/G2. Done: graphs track the console's for the same minute; a null sample shows a gap.
5. **Architect lists.** `a` opens the tabbed view; Decisions, Blockers, Goals, Sessions, Tasks,
   Q&A as read-only tables with expand. Blocked on G3. Read-only first: no `set`/`update`
   from the TUI. Done: counts equal `tm architect dashboard tally` on Studio.
6. **Live tracker tab.** Sessions with nested agents and current step, from the live methods in
   section 5. Not blocked by G1 to G3, so it can ship before slices 3 to 5 if wanted. Done:
   a dispatched agent appears under its session within one poll tick and disappears when done.

Slices 1, 2, 6 are unblocked now except for #10902 (slice 2 falls back to probes). File G1 to G3
before slice 3.

## 9. Open questions for the owner

1. **G1/G2 home.** May `console` serve host metrics and history over its UDS listener
   (an extension of the D8b ruling)? If not, accept `tm tui` sampling the host itself?
2. **Architect store root (G3).** Should the mpm daemon resolve the instance store itself, or
   should `tm` read it from config?
3. **Write access.** Is the Architect view read-only in `tm tui`? The dashboard lets the owner
   answer decisions; answering from the TUI needs `set`/`update` with `if_version` and is a
   separate slice.
4. **Visualize.** Is the Live tracker enough, or should it also carry a small `BarChart`
   (agents or output tokens per session)?
5. **Light theme.** The console has Light, Dark, System. Is dark-only right for the TUI?
6. **Events pill.** `events.health` is held in #10899. Skip the pill until it is ruled on?
