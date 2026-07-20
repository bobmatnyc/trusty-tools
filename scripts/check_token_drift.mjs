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
 * each enforced crate's app.css via block-scoped regex — not a full CSS
 * parser, but sufficient for these small, flat, hand-maintained files (no
 * nested `{}` inside a declaration block). An explicit MAPPING per crate
 * pins which crate custom property corresponds to which canonical
 * `--trusty-*` token (derived from that crate's own app.css comments, which
 * already document the 1:1 correspondence — see #3387/#3153/#3380). Canonical
 * hex is converted to the same `"R G B"` triple format the crates use and
 * diffed per theme. A small number of tokens are carried over verbatim as
 * `rgba(...)` passthroughs rather than converted (e.g. `--trusty-surface-hover`
 * in trusty-agents) — those are diffed as normalized raw strings instead.
 *
 * The 5 pre-Foundry crates (trusty-search, trusty-memory, trusty-mpm-gui,
 * trusty-console, trusty-analyze) are NOT enforced yet — each is tracked in
 * ALLOWLIST below against the issue that migrates it and removes its
 * exemption (#3487/#3488/#3489/#3490).
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
// PR that migrates that crate onto the canonical `--color-*` RGB-triple
// convention. Do not add an entry here without a tracking issue.
// ---------------------------------------------------------------------------
const ALLOWLIST = [
  { crate: "trusty-search", issue: "#3487" },
  { crate: "trusty-memory", issue: "#3487" },
  { crate: "trusty-mpm-gui", issue: "#3488" },
  { crate: "trusty-console", issue: "#3489" },
  { crate: "trusty-analyze", issue: "#3490" },
];

// ---------------------------------------------------------------------------
// Enforced crates + their canonical-token mapping.
// `mappings`: [crateVarSuffix, canonicalTokenName] — crateVarSuffix is the
//   part after `--color-`; canonicalTokenName is the part after `--`.
//   Compared as an RGB triple (canonical hex -> "R G B") per theme.
// `passthrough`: [crateVarName, canonicalTokenName] — full var names,
//   compared as normalized raw rgba(...)/hex strings (no RGB-triple
//   conversion; the crate carries the canonical value over verbatim).
// ---------------------------------------------------------------------------
const ENFORCED = [
  {
    name: "trusty-agents",
    file: "crates/trusty-agents/ui/src/app.css",
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

function parseCanonical(tokensCssPath) {
  const source = readFileSync(tokensCssPath, "utf8");
  const lightBlock = extractBlock(
    source,
    /:root\s*\{([\s\S]*?)\n\}/,
    ":root (light)",
    tokensCssPath,
  );
  const darkBlock = extractBlock(
    source,
    /\[data-theme=(['"])dark\1\][^{]*\{([\s\S]*?)\n\}/,
    "[data-theme='dark'] (dark)",
    tokensCssPath,
  );
  return {
    light: parseDeclarations(lightBlock),
    dark: parseDeclarations(darkBlock),
  };
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

function parseCrate(crate) {
  const filePath = path.join(REPO_ROOT, crate.file);
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

function checkCrate(crate, canonical) {
  const diffs = [];
  const parsed = parseCrate(crate);

  for (const theme of ["light", "dark"]) {
    const canonicalMap = canonical[theme];
    const crateMap = parsed[theme];

    for (const [crateSuffix, canonicalToken] of crate.mappings) {
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
      if (diffs.length === 0) {
        console.log(`OK   ${crate.name} (${crate.file}) — matches canonical`);
      } else {
        console.log(`DRIFT ${crate.name} (${crate.file}) — ${diffs.length} mismatch(es)`);
      }
      allDiffs.push(...diffs);
    } catch (err) {
      console.error(`FATAL: ${crate.name}: ${err.message}`);
      process.exit(1);
    }
  }

  console.log("");
  console.log("Allowlisted (not yet enforced — pending Foundry migration):");
  for (const entry of ALLOWLIST) {
    console.log(`  - ${entry.crate} (${entry.issue})`);
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

main();
