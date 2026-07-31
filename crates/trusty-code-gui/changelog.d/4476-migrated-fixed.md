Fixed

- **App / dock icon — stale "AI COMMANDER" placeholder replaced with the real
  Foundry robot mark.** The entire `icons/` set (`icon.png`/`icon.ico`, every
  PNG size, the `Square*Logo.png` / `StoreLogo.png` tiles) depicted a
  robot-in-a-terminal reading "AI COMMANDER" — a placeholder for a different
  product, shown in the macOS dock and app switcher. It is regenerated from a
  new 1024×1024 master (`icons/icon-master.svg`) derived from the canonical
  Foundry robot mark (`docs/design/UI/design-system/icons/RobotIcon.svelte`,
  the `full` hero variant): a dark oxide squircle (`#2B1C12` / deeper `#241610`
  ground) with the rust-accented (`#b7410e`, the `--trusty-primary` token)
  robot face — antenna, two eyes, `>_` terminal mouth, sparkle. The set was
  regenerated with `cargo tauri icon` (rendered SVG→PNG via `rsvg-convert`),
  which also creates a macOS `icons/icon.icns` (previously absent — the app had
  no `.icns` bundle icon at all). `tauri.conf.json` now wires `bundle.icon`
  explicitly to `32x32.png` / `128x128.png` / `128x128@2x.png` / `icon.icns` /
  `icon.ico` rather than relying on Tauri's implicit lookup. The orphaned,
  unreferenced `icons/aic-logo.svg` (the "AIC" letterform, wrong product) is
  deleted.

- **Header brand mark — placeholder diamond replaced with the canonical
  Foundry robot mark.** `AppHeader.svelte` rendered a literal `◆` diamond
  glyph to the left of the "Trusty Code" wordmark — a placeholder, not the
  brand identity (reported by Bob: the header logo was wrong). It now renders
  the canonical Foundry / Trusty Assistant robot mark, vendored as
  `ui/src/lib/icons/RobotIcon.svelte` from the design-system source
  (`docs/design/UI/design-system/icons/RobotIcon.svelte`, refs #3486/#3495),
  used in its `mono` variant at 18px and colored via the existing
  `text-trusty-primary` header token (through `currentColor`) so it themes in
  light/dark. Scope is the mark only — the wordmark, window title, and layout
  are unchanged.
- **SSE subscription churn in `WorkstreamActivity.svelte` and
  `WorkstreamSwitcher.svelte` (code-critic PR #3392 review, HIGH).** The
  `GET /workstreams/{id}/events` subscription `$effect` in both components
  read the active workstream id through a `$state` object (`activeWorkstream`/
  `list`) that the poll `refresh()` reassigns to a freshly-parsed object
  every tick — Svelte 5 invalidates on reference inequality, so the
  `EventSource` was closing and reopening on EVERY poll tick while a
  workstream was active (~360 reconnects/active-half-hour), dropping any
  event landing in the close→reopen gap and needlessly loading the daemon.
  Both effects now read a `$derived` PRIMITIVE (`activeWorkstreamId`)
  instead, which Svelte 5 only re-runs a dependent for when the VALUE
  actually changes. `WorkstreamSwitcher.svelte`'s instance of this bug
  predates this PR (issue #3300/PR #3356) — fixed here as a carried-in fix
  once the pattern was identified. New reactivity tests in both
  `WorkstreamActivity.test.ts` and `WorkstreamSwitcher.test.ts` stub a fake
  `EventSource` and assert exactly one construction across several
  same-active-id poll ticks (using fake timers; jsdom has no real
  `EventSource` to exercise directly).
- **Activation-fails-after-successful-run left `WorkstreamActivity.svelte`
  claiming "no active workstream" while the task was actually running
  (code-critic PR #3392 review, MEDIUM).** Fixed two ways: `StartWorkingForm.svelte`
  now retries a failed activation ONCE (`activateWithRetry`) before
  surfacing a warning, closing the common transient-failure case silently;
  and a new cross-module store (`lib/pending-workstream.svelte.ts`) records
  the minted-and-run workstream's id/name so `WorkstreamActivity.svelte` can
  fall back to displaying it when the daemon's real active-workstream
  pointer is absent, cleared once activation eventually succeeds.
- **`DEFAULT_DAEMON_URL` moved from `http://127.0.0.1:7881` to
  `http://127.0.0.1:7882`, in lockstep with `trusty-code`'s
  `serve::DEFAULT_HTTP_PORT` (closes #3364).** The old port collided with
  `trusty-mpm`'s supervisor metrics listener, producing "DAEMON UNREACHABLE
  — LOAD FAILED" on a fresh install when both processes were running. A new
  test, `default_daemon_url_matches_tcode_default_http_port`, pins this
  constant against `trusty_code::serve::DEFAULT_HTTP_PORT` directly (new
  dev-dependency on `trusty-code`) so the two cannot drift apart silently
  again. The web-mode fallback — `ui/src/lib/api-config.ts`'s own
  `DEFAULT_DAEMON_URL`, used by `apiBase()` outside the Tauri webview (the
  `pnpm dev` loop and any plain browser tab) — carried a THIRD copy of this
  port that the initial #3364 fix missed; it is now 7882 too, with
  `ui/src/lib/api-config.test.ts` parsing `trusty-code/src/serve/mod.rs`
  directly to assert the two stay in sync.
- GUI-created sessions were inert (closes #3177): the create-session form
  called `POST /sessions` (`session.create`), which only ever minted a
  session record — it never spawned an agent loop, so nothing typed into the
  form actually ran. The form now calls `POST /tasks` (`task.run`, #2983
  Slice 4), the one-shot "mint-or-reuse a session AND start executing" entry
  point (DOC-39 §7A/AC-21), carrying the per-call `project` binding the same
  way (PR #3189's project mismatch `400` surfaces via the form's existing
  generic error-message handling, unchanged). `lib/create-session.ts`'s
  `buildCreateBody`/`extractSessionId` are renamed `buildRunTaskBody`/
  `extractTaskSessionId` to match the new wire shape (`task_description` +
  `project`; response `{session_id, status, mode, binding}`, `202 Accepted`
  instead of `201 Created`), with a new `isTaskRunResponse` runtime shape
  guard replacing the old `POST /sessions`-shaped check.
- GUI smoke-test UX feedback (Bob, 2026-07-18): three fixes to
  `CreateSessionForm.svelte`/`HealthPanel.svelte` and the global theme.
  - **#3132 Enter-to-submit.** The task `<textarea>` had no keyboard submit
    path — click was the only way in. `docs/specs/trusty-code-harness-ui.md`
    specifies no submit-key convention, so this follows the universal
    textarea pattern: `Enter` (without `Shift`, not mid-IME-composition)
    calls `preventDefault()` and submits; `Shift+Enter` is left untouched,
    falling through to the textarea's own newline-insertion default. The
    existing `canSubmitCreate` no-double-submit guard is unchanged — the new
    `handleTaskKeydown` handler defers to the same `submit()` the button
    already called, adding no separate gate. Covered by three new
    `CreateSessionForm.test.ts` cases: Enter submits (and a rapid second
    Enter while the first is in flight produces no second `POST`),
    Shift+Enter never calls `preventDefault()` or submits, and a non-Enter
    key is a no-op.
  - **#3133 light + dark themes following system appearance.** Every
    `trusty-*`/`status-*` Tailwind color token (`tailwind.config.js`) now
    resolves to `rgb(var(--color-*) / <alpha-value>)` instead of a
    hardcoded hex literal; the actual light (default) and
    `@media (prefers-color-scheme: dark)` (override) values live in
    `src/app.css`. No manual toggle — theming purely follows the OS
    setting, live, including inside the Tauri webview (WebKit re-evaluates
    the media query on the native "Appearance changed" notification with no
    reload needed). The previous `darkMode: 'class'` config was dead
    weight — nothing in this codebase ever added a `.dark` class to
    `<html>`, so its `html:not(.dark)` override in `app.css` silently won
    every render regardless of the OS setting. The CSS-variable values are
    space-separated RGB triples (`"15 23 42"`, not `"#0f172a"`) — required
    by the `rgb(var(...) / <alpha-value>)` format, the only way Tailwind
    can still generate the `bg-status-ok/15`/`text-trusty-text/60`-style
    opacity modifiers used throughout every component; a bare `var(--x)`
    reference (this PR's first cut) compiles the base utility fine but
    silently produces NO rule at all for any opacity-modified one. The
    theming audit also caught one real hardcoded color outside the token
    system: `HealthPanel.svelte`'s JSON payload `<pre>` block used the
    Tailwind built-in near-black shade (which never changes with
    `prefers-color-scheme`) instead of a themed token — swapped for the
    `trusty-border` token at reduced opacity. New `lib/theme.test.ts`
    covers the token format, both themes' CSS-variable values (and that
    dark genuinely differs from light), a real `postcss`/`tailwindcss`
    compile check proving opacity-modified utilities produce a rule (the
    actual regression a bare `var()` causes), and a component-hygiene scan
    for `<style>` blocks, hardcoded inline-style colors, and raw
    (non-themed) Tailwind palette utilities.
  - **#3134 picker placement.** The directory listing's `use` button sat at
    the row's far right (`flex-1` on the name button + `justify-between` on
    the row stretched the name button to fill the available width), leaving
    a wide, disorienting gap between a short directory name and the control
    that binds it. The name button is now width-bounded
    (`min-w-0 max-w-[65%]`, `truncate` still applies) rather than
    flex-stretched, so with `justify-between` removed the row's default
    left-packed flex layout puts `use` immediately after the name. Covered
    by a new `CreateSessionForm.test.ts` structural assertion: the `use`
    button must be the name button's next DOM sibling, and neither the name
    button nor the row may carry the `flex-1`/`justify-between` classes that
    caused the separation.
- Create-session flow response-shape hardening (PR #3103 review findings):
  `GET /fs` 200 bodies and `POST /sessions` 201 bodies are now validated at
  runtime (`lib/create-session.ts::isDirListing` / `extractSessionId`)
  instead of bare `as` type assertions. Previously a 200 listing missing
  `entries` threw a `TypeError` out of the `listing.entries.filter(...)`
  derived — and since `CreateSessionForm` is mounted unconditionally, that
  crashed the entire shell; a 201 missing `id` threw the same way from the
  template's `.slice(0, 8)`. Now a shape-invalid listing degrades to the
  existing picker error line ("malformed response from daemon") and a 201
  without a usable id shows a generic "session created" message (the status
  code, not the body, is authoritative that the session exists). Regression
  tests for both live in `CreateSessionForm.test.ts`; guard unit tests in
  `create-session.test.ts`.
- Search-audit shape guard no longer rejects an entire
  `GET /sessions/{id}/search-audit` response over a single bad record (issue
  #3111 MEDIUM, PR #3110 critic review). `lib/search-audit.ts`'s
  `isSearchAuditResponse` (a `body is SearchAuditResponse` all-or-nothing
  predicate) is replaced by `parseSearchAuditResponse`, which filters
  `search_audit` to the subset passing the existing per-record shape check
  and reports how many entries were dropped (`omittedCount`) — a malformed
  row, or a future third `SearchAuditRecord` variant this client build
  doesn't recognize yet, no longer hides every known-good row behind an
  "audit unavailable" wall. Only a genuine top-level shape failure (body not
  an object, or `search_audit` not even an array) still returns `null` and
  renders that error state. `SearchTab.svelte` now renders the trustworthy
  rows plus a visible "N rows omitted (unrecognized record shape)" notice
  when `omittedCount > 0`, rather than silently dropping or wholesale
  rejecting them. Also added a fake-timer-driven regression test (issue
  #3111 LOW) proving the tab recovers real rows on the next poll after a
  transient `HTTP 500` from the audit route.
- Restored the macOS Developer-ID signing config that was intentionally
  omitted at scaffold time, mirroring the `trusty-mpm-gui` pattern (#2951 /
  PR #2957): `tauri.conf.json` now pins `bundle.macOS.signingIdentity` to
  `Developer ID Application: Bob Matsuoka (4JH68XUHC5)` so `cargo tauri build`
  produces a stable-identity `.app` bundle instead of a fresh ad-hoc identity
  per rebuild. `productName`/window title (`"Trusty Code"`) and the bundle
  identifier (`com.trusty.trusty-code.gui`) were already stable from the
  original scaffold and are unchanged. Documented the cert-less
  `APPLE_SIGNING_IDENTITY=- cargo tauri build` escape hatch in the README and
  `docs/reference/common-pitfalls.md`. `trusty-code-gui` was already excluded
  from the workspace's `default-members` at scaffold time (#2983), so no
  change was needed there. `trusty-code`/`tcode` has no `SIGNABLE_BINARIES`
  entry or `tctl sign`-style fallback install script of its own yet, so no
  equivalent install-script wiring was added for `trusty-code-gui` (the Tauri
  config is the only signing path today).
