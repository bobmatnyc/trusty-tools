#!/usr/bin/env node
/**
 * Token drift-check (issue #3486, epic quick-win).
 *
 * Why: docs/design/UI/design-system/tokens.css is the canonical Foundry v2
 * design-token source (hex values, light + `[data-theme='dark']` blocks).
 * crates/trusty-agents/ui/src/app.css and crates/trusty-code-gui/ui/src/app.css
 * both adopted it, but by HAND-TRANSCRIBING the hex into Tailwind's
 * `--color-*: R G B` space-separated RGB-triple convention (consumed as
 * `rgb(var(--color-*) / <alpha-value>)`), so a hand-edit to either side can
 * silently drift the two apart. Nothing previously checked the two stay in
 * sync — `trusty-code-gui/ui/src/lib/theme.test.ts` only asserts internal
 * self-consistency (light/dark blocks both exist and actually differ), not
 * agreement with the canonical source. This script closes that gap.
 *
 * What: parses `:root { ... }` (light) and the dark-theme block (each
 * crate's own dark-activation selector) out of both the canonical file and
 * each enforced crate's token file via block-scoped regex — not a full CSS
 * parser, but sufficient for these small, flat, hand-maintained files (no
 * nested `{}` inside a declaration block). Two comparison MODES exist:
 *
 *   - `rgb-triple` (Tailwind crates: trusty-agents, trusty-code-gui,
 *     trusty-mpm-gui): the crate hand-transcribes canonical hex into
 *     Tailwind's `--color-*: R G B` space-separated triple convention. An
 *     explicit MAPPING per crate pins which `--color-*` var corresponds to
 *     which canonical `--trusty-*` token (derived from that crate's own
 *     app.css comments — see #3387/#3153/#3380). Canonical hex is converted
 *     to the same `"R G B"` triple and diffed per theme. A few tokens are
 *     carried over verbatim as `rgba(...)` passthroughs (e.g.
 *     `--trusty-surface-hover` in trusty-agents) and diffed as normalized
 *     raw strings instead.
 *
 *   - `hex` (plain-CSS crates: trusty-console, trusty-analyze,
 *     trusty-search, trusty-memory): the crate keeps the canonical
 *     `--trusty-*` names verbatim with `#rrggbb` hex values, so no mapping
 *     table or RGB-triple conversion is needed — the crate hex is compared
 *     DIRECTLY (case-insensitively) to the canonical hex of the same token
 *     name. Enforcement is over the INTERSECTION: every canonical *hex*
 *     token the crate also defines must match; crate-only extension tokens
 *     (e.g. trusty-console's `--trusty-status-degraded`) are ignored, and a
 *     crate is never required to define a canonical token it doesn't use.
 *     Non-hex canonical values (rgba/font/spacing) are not diffed in this
 *     mode. Each crate's differing dark/light selectors are configured
 *     per-entry (`:root[data-theme=...]`, `[data-theme=...]`, and
 *     `:root`+`[data-theme='dark']`).
 *
 * All 7 UI crates are now ENFORCED (epic #3486); the ALLOWLIST is empty. A
 * runtime guard throws if any enforced crate performs zero comparisons, so a
 * mis-configured file path or selector can't silently pass.
 *
 * Test: run directly — `node scripts/check_token_drift.mjs` — or via
 * `pnpm run check:tokens` from either enforced crate's `ui/` directory.
 * Exits non-zero with a per-token expected/actual diff on drift, or on any
 * parse failure (missing block/token is treated as a failure, not a silent
 * pass — a structural change to either file should fail loudly).
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..");

const TOKENS_CSS_PATH = path.join(
  REPO_ROOT,
  "docs/design/UI/design-system/tokens.css",
);

// ---------------------------------------------------------------------------
// Pending-migration crates — NOT enforced yet. Each entry is removed by the
// PR that migrates that crate onto canonical Foundry tokens. Do not add an
// entry here without a tracking issue.
//
// EMPTY as of epic #3486: all 7 UI crates are now ENFORCED below (the last 5
// — trusty-search/-memory/-mpm-gui/-console/-analyze, #3487-#3490 — landed
// and were flipped in refs #3486). New pre-Foundry crates would be tracked
// here against a migration issue until they adopt the canonical tokens.
// ---------------------------------------------------------------------------
const ALLOWLIST = [];

// ---------------------------------------------------------------------------
// Enforced crates + their canonical-token comparison config.
//
// `mode`: "rgb-triple" (default) | "hex".
//
// mode "rgb-triple" (Tailwind `--color-*: R G B` crates):
//   `mappings`: [crateVarSuffix, canonicalTokenName] — crateVarSuffix is the
//     part after `--color-`; canonicalTokenName is the part after `--`.
//     Compared as an RGB triple (canonical hex -> "R G B") per theme.
//   `passthrough`: [crateVarName, canonicalTokenName] — full var names,
//     compared as normalized raw rgba(...)/hex strings (no RGB-triple
//     conversion; the crate carries the canonical value over verbatim).
//
// mode "hex" (plain-CSS `--trusty-*: #hex` crates):
//   no mapping table — the crate reuses canonical `--trusty-*` names, so the
//   crate's hex is compared DIRECTLY to canonical hex for every canonical
//   *hex* token the crate also defines (the intersection). Crate-only
//   extension tokens are ignored; non-hex canonical tokens are skipped.
//
// `lightSelector`/`darkSelector`: block-scoped regex for each theme block
//   (the last capture group is the block body).
// ---------------------------------------------------------------------------
const ENFORCED = [
  {
    name: "trusty-agents",
    file: "crates/trusty-agents/ui/src/app.css",
    mode: "rgb-triple",
    lightSelector: /:root\s*\{([\s\S]*?)\n\}/,
    darkSelector: /\.dark\s*\{([\s\S]*?)\n\}/,
    mappings: [
      ["content-bg", "trusty-content-bg"],
      ["card-bg", "trusty-card-bg"],
      ["text-primary", "trusty-text-primary"],
      ["text-muted", "trusty-text-muted"],
      ["border", "trusty-border"],
      ["primary", "trusty-accent"],
      ["info", "trusty-info"],
      ["warning", "trusty-warning"],
      ["sidebar-accent", "trusty-sidebar-accent"],
    ],
    passthrough: [["trusty-surface-hover", "trusty-surface-hover"]],
  },
  {
    name: "trusty-code-gui",
    file: "crates/trusty-code-gui/ui/src/app.css",
    mode: "rgb-triple",
    lightSelector: /:root\s*\{([\s\S]*?)\n\}/,
    darkSelector: /\[data-theme=(['"])dark\1\]\s*\{([\s\S]*?)\n\}/,
    mappings: [
      ["primary", "trusty-accent"],
      ["primary-hover", "trusty-accent-hover"],
      ["surface", "trusty-content-bg"],
      ["card", "trusty-card-bg"],
      ["raised", "trusty-surface-raised"],
      ["border", "trusty-border"],
      ["border-strong", "trusty-border-strong"],
      ["text", "trusty-text-primary"],
      ["text-secondary", "trusty-text-secondary"],
      ["text-muted", "trusty-text-muted"],
      ["text-inverse", "trusty-text-inverse"],
      ["status-ok", "trusty-success"],
      ["status-error", "trusty-danger"],
      ["status-warn", "trusty-warning"],
      ["status-neutral", "trusty-text-muted"],
      ["sidebar-bg", "trusty-sidebar-bg"],
      ["sidebar-text", "trusty-sidebar-text"],
      ["sidebar-muted", "trusty-sidebar-muted"],
      ["sidebar-border", "trusty-sidebar-border"],
      ["sidebar-active", "trusty-sidebar-active"],
    ],
    passthrough: [],
  },
  {
    // Same Tailwind `--color-*: R G B` convention as trusty-code-gui (#3488).
    name: "trusty-mpm-gui",
    file: "crates/trusty-mpm-gui/ui/src/app.css",
    mode: "rgb-triple",
    lightSelector: /:root\s*\{([\s\S]*?)\n\}/,
    darkSelector: /\[data-theme=(['"])dark\1\]\s*\{([\s\S]*?)\n\}/,
    mappings: [
      ["primary", "trusty-accent"],
      ["primary-hover", "trusty-accent-hover"],
      ["surface", "trusty-content-bg"],
      ["card", "trusty-card-bg"],
      ["raised", "trusty-surface-raised"],
      ["border", "trusty-border"],
      ["border-strong", "trusty-border-strong"],
      ["text", "trusty-text-primary"],
      ["text-secondary", "trusty-text-secondary"],
      ["text-muted", "trusty-text-muted"],
      ["text-inverse", "trusty-text-inverse"],
      ["status-ok", "trusty-success"],
      ["status-error", "trusty-danger"],
      ["status-warn", "trusty-warning"],
      ["status-neutral", "trusty-text-muted"],
    ],
    passthrough: [],
  },
  {
    // Plain CSS `--trusty-*: #hex`, light/dark under
    // `:root[data-theme="light"]` / `:root[data-theme="dark"]` (#3489).
    // Crate-only extensions (`--trusty-status-degraded`, `--trusty-status-absent`)
    // are ignored by the intersection rule.
    name: "trusty-console",
    file: "crates/trusty-console/ui/src/theme.css",
    mode: "hex",
    lightSelector: /:root\[data-theme="light"\]\s*\{([\s\S]*?)\n\}/,
    darkSelector: /:root\[data-theme="dark"\]\s*\{([\s\S]*?)\n\}/,
  },
  {
    // Plain CSS `--trusty-*: #hex`. Light under `[data-theme="light"]`; dark
    // (the default) under the combined `:root, [data-theme="dark"]` selector.
    // A trailing `:root { ... }` alias layer repoints crate-local names to
    // `--trusty-*` — not matched by either theme selector (#3490).
    name: "trusty-analyze",
    file: "crates/trusty-analyze/ui/src/lib/styles/tokens.css",
    mode: "hex",
    lightSelector: /\[data-theme="light"\]\s*\{([\s\S]*?)\n\}/,
    darkSelector: /:root\s*,\s*\[data-theme="dark"\]\s*\{([\s\S]*?)\n\}/,
  },
  {
    // Plain CSS `--trusty-*: #hex`, light under `:root`, dark under
    // `[data-theme='dark']` (#3487).
    name: "trusty-search",
    file: "crates/trusty-search/ui/src/lib/styles/tokens.css",
    mode: "hex",
    lightSelector: /:root\s*\{([\s\S]*?)\n\}/,
    darkSelector: /\[data-theme=(['"])dark\1\]\s*\{([\s\S]*?)\n\}/,
  },
  {
    // Identical token file + selectors to trusty-search (#3487).
    name: "trusty-memory",
    file: "crates/trusty-memory/ui/src/lib/styles/tokens.css",
    mode: "hex",
    lightSelector: /:root\s*\{([\s\S]*?)\n\}/,
    darkSelector: /\[data-theme=(['"])dark\1\]\s*\{([\s\S]*?)\n\}/,
  },
];

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/** Strip /* ... *\/ block comments so they can't confuse declaration parsing. */
function stripComments(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, "");
}

/** Extract a `{ ... }` block's inner text via a block-scoped regex. */
function extractBlock(source, regex, label, filePath) {
  const match = regex.exec(source);
  if (!match) {
    throw new Error(
      `could not find ${label} block in ${filePath} (regex ${regex}) — ` +
        `file structure changed; update check_token_drift.mjs`,
    );
  }
  // Last capture group holds the block body regardless of how many groups
  // the selector regex needed (e.g. a quote-matching backreference group).
  return match[match.length - 1];
}

/** Parse `--name: value;` declarations out of block text into a Map. */
function parseDeclarations(blockText) {
  const decls = new Map();
  const re = /--([a-z0-9-]+)\s*:\s*([^;]+);/gi;
  let m;
  for (const clean = stripComments(blockText); (m = re.exec(clean)); ) {
    decls.set(m[1], m[2].trim());
  }
  return decls;
}

function hexToRgbTriple(hex) {
  const clean = hex.replace("#", "");
  if (!/^[0-9a-fA-F]{6}$/.test(clean)) {
    throw new Error(`not a 6-digit hex color: "${hex}"`);
  }
  const r = parseInt(clean.slice(0, 2), 16);
  const g = parseInt(clean.slice(2, 4), 16);
  const b = parseInt(clean.slice(4, 6), 16);
  return `${r} ${g} ${b}`;
}

function rgbTripleToHex(triple) {
  const parts = triple.trim().split(/\s+/).map(Number);
  if (parts.length !== 3 || parts.some((n) => Number.isNaN(n))) {
    throw new Error(`not a valid "R G B" triple: "${triple}"`);
  }
  return (
    "#" +
    parts.map((n) => n.toString(16).padStart(2, "0")).join("")
  );
}

/** True iff `raw` is a 6-digit `#rrggbb` hex color (ignoring surrounding ws). */
function isHex6(raw) {
  return /^#[0-9a-fA-F]{6}$/.test(raw.trim());
}

/** Normalize a 6-digit hex for case-insensitive comparison. */
function normalizeHex(raw) {
  return raw.trim().toLowerCase();
}

function normalizeTriple(raw) {
  const parts = raw.trim().split(/\s+/).map(Number);
  if (parts.length !== 3 || parts.some((n) => Number.isNaN(n))) {
    throw new Error(`not a valid "R G B" triple: "${raw}"`);
  }
  return parts.join(" ");
}

/** Normalize an rgba()/rgb()/hex passthrough value for string comparison. */
function normalizeColorString(raw) {
  const trimmed = raw.trim();
  const fnMatch = /^rgba?\(([^)]+)\)$/i.exec(trimmed);
  if (fnMatch) {
    const nums = fnMatch[1]
      .split(",")
      .map((s) => s.trim())
      .map((s) => (s.includes(".") ? parseFloat(s) : parseInt(s, 10)));
    const fnName = trimmed.slice(0, trimmed.indexOf("(")).toLowerCase();
    return `${fnName}(${nums.join(", ")})`;
  }
  if (trimmed.startsWith("#")) {
    return trimmed.toLowerCase();
  }
  return trimmed;
}

// ---------------------------------------------------------------------------
// Canonical source parsing
// ---------------------------------------------------------------------------

// Pure — takes canonical CSS source text directly (no file I/O), so tests
// can exercise the real parsing/matching logic against small inline
// fixtures instead of the actual tokens.css.
function parseCanonicalFromSource(source, sourceLabel = TOKENS_CSS_PATH) {
  const lightBlock = extractBlock(
    source,
    /:root\s*\{([\s\S]*?)\n\}/,
    ":root (light)",
    sourceLabel,
  );
  const darkBlock = extractBlock(
    source,
    /\[data-theme=(['"])dark\1\][^{]*\{([\s\S]*?)\n\}/,
    "[data-theme='dark'] (dark)",
    sourceLabel,
  );
  return {
    light: parseDeclarations(lightBlock),
    dark: parseDeclarations(darkBlock),
  };
}

function parseCanonical(tokensCssPath) {
  const source = readFileSync(tokensCssPath, "utf8");
  return parseCanonicalFromSource(source, tokensCssPath);
}

function canonicalRgbTriple(canonicalMap, token, ctx) {
  if (!canonicalMap.has(token)) {
    throw new Error(
      `canonical token "--${token}" not found in ${ctx} block of ` +
        `${TOKENS_CSS_PATH} — mapping in check_token_drift.mjs is stale`,
    );
  }
  const raw = canonicalMap.get(token);
  if (!raw.startsWith("#")) {
    throw new Error(
      `canonical token "--${token}" (${ctx}) is not a hex value ("${raw}") ` +
        `— it can't be used in an RGB-triple mapping; use "passthrough" instead`,
    );
  }
  return hexToRgbTriple(raw);
}

// ---------------------------------------------------------------------------
// Crate parsing + diffing
// ---------------------------------------------------------------------------

// `crate.file` is relative-to-REPO_ROOT for every real ENFORCED entry; tests
// pass an absolute path to a temp fixture instead, so resolve conditionally
// rather than blindly `path.join`-ing (path.join does NOT treat a second
// absolute segment as a root reset).
function resolveCrateFilePath(crate) {
  return path.isAbsolute(crate.file)
    ? crate.file
    : path.join(REPO_ROOT, crate.file);
}

function parseCrate(crate) {
  const filePath = resolveCrateFilePath(crate);
  const source = readFileSync(filePath, "utf8");
  const lightBlock = extractBlock(
    source,
    crate.lightSelector,
    ":root (light)",
    filePath,
  );
  const darkBlock = extractBlock(
    source,
    crate.darkSelector,
    "dark",
    filePath,
  );
  return {
    filePath,
    light: parseDeclarations(lightBlock),
    dark: parseDeclarations(darkBlock),
  };
}

// `parsedOverride`, when supplied, is used instead of reading+parsing
// `crate.file` from disk — lets tests exercise the real comparison logic
// against synthetic {light, dark} declaration maps without touching the
// filesystem at all.
function checkCrate(crate, canonical, parsedOverride) {
  const mode = crate.mode ?? "rgb-triple";

  // Config-level invariant for rgb-triple crates: a crate with zero
  // comparisons CONFIGURED is not "verified clean" — it's unverified. Without
  // this guard, an ENFORCED entry whose `mappings`/`passthrough` were
  // accidentally (or maliciously) emptied out would silently report
  // "OK ... matches canonical" and exit 0 regardless of what the crate's
  // app.css actually contains — the exact false-green this whole script
  // exists to prevent. (hex crates have no static mapping table; they are
  // guarded by the runtime `comparedCount` check below instead.)
  if (mode === "rgb-triple") {
    const tokenCount = crate.mappings.length + crate.passthrough.length;
    if (tokenCount === 0) {
      throw new Error(
        `mappings+passthrough is empty — refusing to report a pass with ` +
          `zero comparisons`,
      );
    }
  }

  const diffs = [];
  const parsed = parsedOverride ?? parseCrate(crate);
  // Runtime guard: however the crate is configured, if the actual comparison
  // pass touches zero tokens (e.g. a mis-configured selector matched a block
  // with no canonical tokens in it), that's an unverified pass, not a clean
  // one — throw loudly like every other structural failure.
  let comparedCount = 0;

  if (mode === "hex") {
    // Plain-CSS-hex: compare the crate's own `--trusty-*: #hex` values
    // DIRECTLY to canonical hex, over the INTERSECTION of tokens the crate
    // defines and canonical defines-as-hex. Crate-only extension tokens and
    // non-hex canonical values (rgba/font/spacing) are ignored.
    for (const theme of ["light", "dark"]) {
      const canonicalMap = canonical[theme];
      const crateMap = parsed[theme];
      for (const [name, crateRaw] of crateMap) {
        if (!canonicalMap.has(name)) continue; // crate-only extension token
        const canonRaw = canonicalMap.get(name);
        if (!isHex6(canonRaw)) continue; // only enforce hex-valued canonical tokens
        comparedCount++;
        const expected = normalizeHex(canonRaw);
        const actual = isHex6(crateRaw) ? normalizeHex(crateRaw) : crateRaw.trim();
        if (actual !== expected) {
          diffs.push({
            crate: crate.name,
            theme,
            var: `--${name}`,
            canonicalToken: name,
            expected,
            actual,
          });
        }
      }
    }
    diffs.comparedCount = comparedCount;
    if (comparedCount === 0) {
      throw new Error(
        `${crate.name}: zero token comparisons performed (hex mode) — the ` +
          `configured file/selectors matched no canonical hex tokens; ` +
          `check ${crate.file} and the light/dark selectors`,
      );
    }
    return diffs;
  }

  for (const theme of ["light", "dark"]) {
    const canonicalMap = canonical[theme];
    const crateMap = parsed[theme];

    for (const [crateSuffix, canonicalToken] of crate.mappings) {
      comparedCount++;
      const crateVar = `color-${crateSuffix}`;
      const expected = canonicalRgbTriple(canonicalMap, canonicalToken, theme);

      if (!crateMap.has(crateVar)) {
        diffs.push({
          crate: crate.name,
          theme,
          var: `--${crateVar}`,
          canonicalToken,
          expected: `${expected} (${rgbTripleToHex(expected)})`,
          actual: "<missing>",
        });
        continue;
      }

      const actualRaw = crateMap.get(crateVar);
      const actual = normalizeTriple(actualRaw);
      if (actual !== expected) {
        diffs.push({
          crate: crate.name,
          theme,
          var: `--${crateVar}`,
          canonicalToken,
          expected: `${expected} (${rgbTripleToHex(expected)})`,
          actual: `${actual} (${rgbTripleToHex(actual)})`,
        });
      }
    }

    for (const [crateVarName, canonicalToken] of crate.passthrough) {
      comparedCount++;
      if (!canonicalMap.has(canonicalToken)) {
        throw new Error(
          `canonical token "--${canonicalToken}" (${theme}) not found for ` +
            `passthrough mapping in ${crate.name} — mapping is stale`,
        );
      }
      const expectedRaw = canonicalMap.get(canonicalToken);
      const expected = normalizeColorString(expectedRaw);

      if (!crateMap.has(crateVarName)) {
        diffs.push({
          crate: crate.name,
          theme,
          var: `--${crateVarName}`,
          canonicalToken,
          expected,
          actual: "<missing>",
        });
        continue;
      }

      const actual = normalizeColorString(crateMap.get(crateVarName));
      if (actual !== expected) {
        diffs.push({
          crate: crate.name,
          theme,
          var: `--${crateVarName}`,
          canonicalToken,
          expected,
          actual,
        });
      }
    }
  }

  diffs.comparedCount = comparedCount;
  if (comparedCount === 0) {
    throw new Error(
      `${crate.name}: zero token comparisons performed — ` +
        `check ${crate.file} and its selectors`,
    );
  }
  return diffs;
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  console.log(`Canonical source: ${path.relative(REPO_ROOT, TOKENS_CSS_PATH)}`);
  console.log("");

  let canonical;
  try {
    canonical = parseCanonical(TOKENS_CSS_PATH);
  } catch (err) {
    console.error(`FATAL: failed to parse canonical tokens.css: ${err.message}`);
    process.exit(1);
  }

  const allDiffs = [];
  for (const crate of ENFORCED) {
    try {
      const diffs = checkCrate(crate, canonical);
      const mode = crate.mode ?? "rgb-triple";
      const n = diffs.comparedCount ?? 0;
      if (diffs.length === 0) {
        console.log(
          `OK   ${crate.name} (${crate.file}) — ${n} tokens match canonical [${mode}]`,
        );
      } else {
        console.log(
          `DRIFT ${crate.name} (${crate.file}) — ${diffs.length} mismatch(es) of ${n} compared [${mode}]`,
        );
      }
      allDiffs.push(...diffs);
    } catch (err) {
      console.error(`FATAL: ${crate.name}: ${err.message}`);
      process.exit(1);
    }
  }

  console.log("");
  if (ALLOWLIST.length > 0) {
    console.log("Allowlisted (not yet enforced — pending Foundry migration):");
    for (const entry of ALLOWLIST) {
      console.log(`  - ${entry.crate} (${entry.issue})`);
    }
  } else {
    console.log("Allowlist empty — all UI crates are enforced.");
  }

  if (allDiffs.length > 0) {
    console.log("");
    console.error(`TOKEN DRIFT DETECTED (${allDiffs.length} mismatch(es)):`);
    for (const d of allDiffs) {
      console.error(
        `  [${d.crate}/${d.theme}] ${d.var} (canonical --${d.canonicalToken}): ` +
          `expected "${d.expected}", found "${d.actual}"`,
      );
    }
    console.error("");
    console.error(
      "Fix: update the crate's app.css value(s) to match docs/design/UI/design-system/tokens.css, " +
        "or if the canonical file changed intentionally, this diff confirms the crate needs updating too.",
    );
    process.exit(1);
  }

  console.log("");
  console.log("All enforced crates match the canonical token source.");
}

// Only run the CLI when executed directly (`node check_token_drift.mjs`),
// not when imported by scripts/check_token_drift.test.mjs — an import must
// not have the side effect of running the full check and calling
// `process.exit()`.
const isMainModule = import.meta.url === `file://${process.argv[1]}`;
if (isMainModule) {
  main();
}

export {
  ENFORCED,
  ALLOWLIST,
  REPO_ROOT,
  TOKENS_CSS_PATH,
  hexToRgbTriple,
  rgbTripleToHex,
  isHex6,
  normalizeHex,
  normalizeTriple,
  normalizeColorString,
  parseDeclarations,
  extractBlock,
  parseCanonical,
  parseCanonicalFromSource,
  parseCrate,
  checkCrate,
  main,
};
