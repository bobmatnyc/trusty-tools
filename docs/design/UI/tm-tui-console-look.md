# tm tui: console look and Architect view

Status: owner questions Q1-Q5 resolved (see section 9). Goal `ar-G01` (Bob, 2026-11-10), tracked in #10966. Scope: design only.
Foundry (`README.md` here) is the web design system; this note maps its Night Shift dark
palette and the console Overview layout onto a ratatui screen. No new design tokens.

Constraints (owner rulings, 2026-11-09): `tm tui` uses UDS only, no native HTTP (#10899).
Data comes from existing daemon methods and the console UDS listener (#10902, merged). Keep it simple.
v1 has no write path and no charts (ruling Dc4, 2026-11-10 15:21Z, "A for all three").

## 1. Where `tm tui` stands (read at `525f15cf80`)

- `crates/trusty-mpm/src/tui/home/` is the opening screen: a 4-column tile grid
  (`home/render.rs::render_grid`), one tile per service, a service view on Enter.
  Tile status comes from UDS `<svc>.health` probes (`home/services.rs::probe`). Agents and
  Console are no longer `HttpAddr` (#10903); with no UDS endpoint they show "no UDS endpoint", not Down.
- Per-service metrics are a stdio spawn of `console_metrics` (`home/probe.rs`), cached 30 s.
  There is no history, so no sparklines.
- `tui/project_ctl/` is the projects view (Enter on the mpm tile). Since #10754 it is two
  columns: Owner -> Project tree left (30%), the node's sessions right (`project_ctl/layout.rs::main_columns`),
  with an Activity strip and an action bar. It reads the mpm daemon through `DaemonClient`,
  which has a unix-socket mode (`socket: Option<PathBuf>`, #6288).
- Colors are bare ANSI names (`Color::Green`, `Color::DarkGray`) in about 10 files. There is no
  theme module.
- ratatui is 0.30 (workspace `Cargo.toml`), feature `tui`. Its widget crate ships `Sparkline`,
  `Gauge`, `Table`, `Tabs`, `Block`, `Scrollbar`, `Canvas`.

## 2. Where the console gets its data today

| Console element | Source (HTTP, browser) | UDS-reachable? |
|---|---|---|
| Machine cards: CPU, memory, swap, disk, network, pressure badge | `GET /api/console/machine-status` | No (gap #10942) |
| Card sparklines (1 Hz ring, 600 samples) | `GET /api/console/machine-status/history`, SSE `/stream` (`machine_history::HistorySnapshot.samples`) | No (#10942) |
| Services roster, version, status badge | `GET /api/console/services` | Yes, `console.fleet_status` (#10902, merged) |
| Per-service CPU/MEM last 10 min | same snapshot, `service_samples` (`{id, status, cpu_pct, rss_bytes}`) | No (#10943) |
| Per-service detail metrics | stdio `console_metrics` | Yes (`<svc>.console_metrics`, `by_*`; #10906 / #10907) |
| Architect link | `GET /api/console/architect-dashboard` | Not needed; see section 4 |

The history rings live only in the console process (`crates/trusty-console/src/machine_history/`).
The #10902 listener serves `console.health` and `console.fleet_status` and nothing else; #10907 adds
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

Opened with `a` from Home. The view is read-only (Dc4): no answer, `set` or `update` from the TUI. Tabs across the top mirror the dashboard categories, with open
counts: Decisions, Actions, Goals, Sessions, Tasks, Q&A, and **Live** in place of Visualize.
Lists are `Table`/`List` with an id column, a title and a status badge; Enter expands a row
into a detail pane below; Esc returns.

```
 ARCHITECT   Decisions 3 · Actions 1 · Goals 7 · Sessions 12 · Tasks 9 · Q&A 2 · [Live]
 ┌ Live ─────────────────────────────────────────────────────────────────────────┐
 │ ● tm-03            goal G01   working     ctx 41%   waiting on: -            │
 │ │  ├ engineer      tm tui theme module            running 6m                 │
 │ │  └ qa            cargo test -p trusty-mpm       running 1m                 │
 │ ◐ ar-pm-02         goal G04   waiting-ci  ctx 22%   PR #10931                │
 │ │  └ (no agents)   idle · last relay 4m ago                                  │
 │ ○ ts-01            goal G02   idle        ctx 63%                            │
 │ │  └ (no agents)                                                             │
 └───────────────────────────────────────────────────────────────────────────────┘
 Decisions 3 open · Actions 1 · Tasks 9 open · Q&A 2 waiting          [Tab] next tab
```

The Live tab is a vertical tracker: one row per session (state glyph, name, goal, state word,
context %, waiting-on), the session's running agents indented below it as a tree, each with the
task text and elapsed time. It redraws on the poll tick; a collapsed session shows only its
row. This is a `List` of styled `Line`s, list-only (Dc4): no graph widget and no chart.

## 5. Data source per panel

| Panel | UDS method | Status |
|---|---|---|
| Header, version, Console badge | `console.health` | Exists (#10902, merged) |
| Services table: name, version, status | `console.fleet_status`; fallback per-service `<svc>.health` probes | Exists (#10902, merged) |
| Services %CPU / MEM now | `<svc>.console_metrics` for service-specific values | Exists (#10906 / #10907); **host-process CPU/RSS is not in any of them (#10943)** |
| Machine cards (CPU, mem, swap, disk, net, pressure) | none | **#10942** |
| Card and service sparklines (10 min) | none | **#10942, #10943** |
| Architect Decisions / Actions / Goals / Sessions / Tasks / Q&A | `mpm.architect.dashboard.list` (`collection`, `status`, `order_by`), `.tally`, `.footer` | Exists (`trusty-architect` `dashboard::rpc::METHODS`); needs the store root (#10944) |
| Live: sessions | `mpm.sessions.list` or `mpm.managed.fleet` | Exists |
| Live: agents per session | `mpm.delegation.list` (`Delegation { session, agent, task, status, created_at }`) | Exists; confirm the TUI-callable shape in S4 |
| Live: current step per session | `mpm.managed.activity` (`state`, `summary`) | Exists; polled for the selected session only today |
| Events / bus health pill | `events.health` | Exists (#10905, merged); shown as the events pill |

Gaps, each filed as its own issue:

- **#10942.** `console.machine_status` and `console.history` on the #10902 listener,
  returning what `routes/machine_status.rs` and `routes/machine_history.rs` serve today
  (`HostMetrics` plus the `samples` ring). Ruled in Q1 (below).
- **#10943.** The same history call must include `service_samples`, or `console.fleet_status` must
  carry the latest sample per service. Without it the per-service sparklines have no source.
- **#10944.** The Architect store root. The dashboard methods take `store {root, format}` on every
  call. Ruled in Q2: the daemon resolves the instance store; `--architect-store` stays as an override.
- **Open (inference, not confirmed).** The Architect dashboard's session `details.current` and
  `usage.agents_active` are written by the Architect poller into the store; the Live tab can
  take them from the `sessions` collection if `mpm.delegation.list` is too coarse for a
  "current step". Decide after reading live output in S4.

Sampling the host from `tm tui` with `sysinfo` would duplicate the console sampler and diverge
from "data through trusty-console", so it is not the design.

## 6. Crate choices

- **ratatui 0.30 built-ins: `Sparkline`, `Table`, `Block`, `Tabs`, `Gauge`.** Already a
  dependency. `Sparkline` covers every console graph (a bar series with eight height levels).
  No `Chart` or `BarChart` in v1 (Dc4). No new dependency.
- **No `tui-big-text`.** The "big value" is a bold line, 1 row, in the accent color. A 3-row
  font costs a crate and card height for no information.
- **No `sysinfo` in the TUI.** Host numbers come from console (#10942).
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
stays, to avoid a black box on a light terminal. The palette is dark only (Dc4); there is no light or system theme.

## 8. Slice plan

Tracked in #10966. One PR per slice. Every slice's done check: a screenshot or asciinema of
`tm tui` next to the console at the same terminal width, attached to the PR. Ladder rung 3 for
rendering slices (one crate), rung 5 for the server slice that touches a method contract.

- **S0. This note**, aligned with Dc4. Docs only; merges first.
- **S1. Theme module and recolour.** `tui/theme.rs`, color-depth detection (truecolor, 256,
  16, `NO_COLOR`), badge helper; existing panes adopt it. Unit tests for the three depths and
  `NO_COLOR`.
- **S2. Home console look.** Header, MACHINE STATUS cards and SERVICES table replace the tile
  grid. Cards and history columns read "no data yet" until S5 and S6. Agents and Console keep
  "no UDS endpoint" (#10903) until S5.
- **S3. Architect view.** `a` opens six read-only tabs (Decisions, Actions, Goals, Sessions,
  Tasks, Q&A) with a detail pane and a tally footer. Store root from `--architect-store` until S7.
  Done: counts equal `tm architect dashboard tally`.
- **S4. Live tracker.** List-only tab (section 4), from `mpm.sessions.list`,
  `mpm.delegation.list` and `mpm.managed.activity`. Done: a dispatched agent appears under its
  session within one poll tick and disappears when done.
- **S5. Console UDS client.** Services table from `console.fleet_status`, header from
  `console.health`. Console moves from "no UDS endpoint" to a real probe.
- **S6. Machine cards and sparklines.** Server PR first: `console.machine_status`,
  `console.history` (#10942) and `service_samples` (#10943), rung 5. Then the TUI PR fills the
  cards and sparklines. A null sample draws a gap.
- **S7. Drop the store-root prompt.** After #10944 the daemon resolves the store root;
  `--architect-store` stays as an override.

## 9. Owner rulings

- **Q1. Host metrics over console UDS.** Yes: `console.machine_status` and `console.history`
  (#10942, #10943). Ruled 2026-11-10 01:01Z.
- **Q2. Architect store root.** The mpm daemon resolves it (#10944). Ruled 2026-11-10 01:01Z.
- **Q3. Write access.** The Architect view is read-only. Dc4, 2026-11-10 15:21Z.
- **Q4. Visualize.** The Live tracker is list-only, no `BarChart`. Dc4, 2026-11-10 15:21Z.
- **Q5. Theme.** Dark console palette with a 16-color fallback; no light theme. Dc4,
  2026-11-10 15:21Z.
