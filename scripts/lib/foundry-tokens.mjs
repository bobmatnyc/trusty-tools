/**
 * Foundry design-token core — canonical parsing, conversion, and the consumer
 * profile table (issue #5095).
 *
 * Why: `docs/design/UI/design-system/tokens.css` is the canonical Foundry hex
 * source, but Tailwind consumers need each color as a space-separated RGB
 * triple (`--color-x: R G B`) so `rgb(var(--color-x) / <alpha-value>)` can
 * generate opacity-modified utilities. Until #5095 that conversion was done by
 * hand in `crates/trusty-agents/ui/src/app.css`, with
 * `scripts/check_token_drift.mjs` only checking after the fact that the hand
 * copy still agreed. One hand-maintained copy was tolerable; the website
 * (#5092) makes it two. This module holds the ONE implementation both the
 * generator and the drift checker route through, so the mapping table cannot
 * fork.
 *
 * What: pure functions — no CLI, no writes. Parses the canonical file's light
 * and dark blocks into declaration maps, converts hex to/from RGB triples, and
 * renders a consumer's generated CSS from `CONSUMERS`.
 *
 * Test: `node --test scripts/generate_foundry_tokens.test.mjs` and
 * `node --test scripts/check_token_drift.test.mjs` (both exercise these
 * helpers against inline fixtures rather than the real tokens.css).
 */

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));

export const REPO_ROOT = path.resolve(__dirname, "../..");

export const TOKENS_CSS_PATH = path.join(
  REPO_ROOT,
  "docs/design/UI/design-system/tokens.css",
);

/** Path of the canonical source relative to the repo root, for messages. */
export const TOKENS_CSS_REL = path.relative(REPO_ROOT, TOKENS_CSS_PATH);

// ---------------------------------------------------------------------------
// Consumer profiles
//
// One entry per generated-token consumer. Adding a consumer (e.g. `website/`,
// #5092) means adding an entry here and running the generator — never
// hand-writing a second RGB-triple block.
//
//   `outFile`     repo-root-relative path of the generated CSS.
//   `mappings`    [consumerVarSuffix, canonicalToken] — the suffix after
//                 `--color-`; emitted as a `"R G B"` triple converted from the
//                 canonical token's hex, per theme.
//   `passthrough` [consumerVarName, canonicalToken] — full var names, emitted
//                 VERBATIM. For canonical values that already bake their own
//                 alpha (`rgba(...)`) and so cannot take Tailwind's
//                 `<alpha-value>`; read directly via `var()` in component
//                 styles.
//   `lightSelector`/`darkSelector`  the consumer's own theme-activation
//                 selectors. trusty-agents uses `.dark` (its `stores/theme.ts`
//                 toggles that class, matching `darkMode: 'class'`), not the
//                 canonical file's `[data-theme='dark']`.
//
// `:root` and `.dark` have EQUAL specificity (0,1,0), so on `<html class="dark">`
// the later block wins on source order alone. The renderer always emits light
// before dark, and both live in one file with nothing interposed.
// ---------------------------------------------------------------------------
export const CONSUMERS = [
  {
    name: "trusty-agents",
    outFile: "crates/trusty-agents/ui/src/lib/styles/foundry-tokens.generated.css",
    consumerDoc: "crates/trusty-agents/ui/src/app.css",
    lightSelector: ":root",
    darkSelector: ".dark",
    mappings: [
      ["content-bg", "trusty-content-bg"],
      ["card-bg", "trusty-card-bg"],
      ["text-primary", "trusty-text-primary"],
      ["text-muted", "trusty-text-muted"],
      ["border", "trusty-border"],
      ["primary", "trusty-accent"],
      ["info", "trusty-info"],
      ["success", "trusty-success"],
      ["warning", "trusty-warning"],
      ["sidebar-accent", "trusty-sidebar-accent"],
    ],
    passthrough: [["trusty-surface-hover", "trusty-surface-hover"]],
  },
];

/** Look up a consumer profile by name, or throw listing the valid names. */
export function consumerByName(name) {
  const found = CONSUMERS.find((c) => c.name === name);
  if (!found) {
    throw new Error(
      `unknown consumer "${name}" — known: ${CONSUMERS.map((c) => c.name).join(", ")}`,
    );
  }
  return found;
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/** Strip `/* ... *\/` block comments so they can't confuse declaration parsing. */
export function stripComments(text) {
  return text.replace(/\/\*[\s\S]*?\*\//g, "");
}

/** Extract a `{ ... }` block's inner text via a block-scoped regex. */
export function extractBlock(source, regex, label, filePath) {
  const match = regex.exec(source);
  if (!match) {
    throw new Error(
      `could not find ${label} block in ${filePath} (regex ${regex}) — ` +
        `file structure changed; update scripts/lib/foundry-tokens.mjs`,
    );
  }
  // Last capture group holds the block body regardless of how many groups the
  // selector regex needed (e.g. a quote-matching backreference group).
  return match[match.length - 1];
}

/**
 * Block-scoped regex for a simple selector string (`:root`, `.dark`), so a
 * consumer profile can state its selectors once as plain CSS and both the
 * renderer and the drift checker derive the same matcher from it.
 */
export function blockRegexFor(selector) {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(`${escaped}\\s*\\{([\\s\\S]*?)\\n\\}`);
}

/** Parse `--name: value;` declarations out of block text into a Map. */
export function parseDeclarations(blockText) {
  const decls = new Map();
  const re = /--([a-z0-9-]+)\s*:\s*([^;]+);/gi;
  let m;
  for (const clean = stripComments(blockText); (m = re.exec(clean)); ) {
    decls.set(m[1], m[2].trim());
  }
  return decls;
}

export function hexToRgbTriple(hex) {
  const clean = hex.replace("#", "");
  if (!/^[0-9a-fA-F]{6}$/.test(clean)) {
    throw new Error(`not a 6-digit hex color: "${hex}"`);
  }
  const r = parseInt(clean.slice(0, 2), 16);
  const g = parseInt(clean.slice(2, 4), 16);
  const b = parseInt(clean.slice(4, 6), 16);
  return `${r} ${g} ${b}`;
}

export function rgbTripleToHex(triple) {
  const parts = triple.trim().split(/\s+/).map(Number);
  if (parts.length !== 3 || parts.some((n) => Number.isNaN(n))) {
    throw new Error(`not a valid "R G B" triple: "${triple}"`);
  }
  return "#" + parts.map((n) => n.toString(16).padStart(2, "0")).join("");
}

/** True iff `raw` is a 6-digit `#rrggbb` hex color (ignoring surrounding ws). */
export function isHex6(raw) {
  return /^#[0-9a-fA-F]{6}$/.test(raw.trim());
}

/** Normalize a 6-digit hex for case-insensitive comparison. */
export function normalizeHex(raw) {
  return raw.trim().toLowerCase();
}

export function normalizeTriple(raw) {
  const parts = raw.trim().split(/\s+/).map(Number);
  if (parts.length !== 3 || parts.some((n) => Number.isNaN(n))) {
    throw new Error(`not a valid "R G B" triple: "${raw}"`);
  }
  return parts.join(" ");
}

/** Normalize an rgba()/rgb()/hex passthrough value for string comparison. */
export function normalizeColorString(raw) {
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

/**
 * Parse canonical CSS source text into `{ light, dark }` declaration maps.
 * Pure — takes source text, not a path, so tests can use inline fixtures.
 */
export function parseCanonicalFromSource(source, sourceLabel = TOKENS_CSS_PATH) {
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

export function parseCanonical(tokensCssPath = TOKENS_CSS_PATH) {
  return parseCanonicalFromSource(
    readFileSync(tokensCssPath, "utf8"),
    tokensCssPath,
  );
}

/** Canonical hex for `token` in `theme`, as an `"R G B"` triple. */
export function canonicalRgbTriple(canonicalMap, token, theme) {
  if (!canonicalMap.has(token)) {
    throw new Error(
      `canonical token "--${token}" not found in the ${theme} block of ` +
        `${TOKENS_CSS_REL} — the consumer mapping is stale`,
    );
  }
  const raw = canonicalMap.get(token);
  if (!raw.startsWith("#")) {
    throw new Error(
      `canonical token "--${token}" (${theme}) is not a hex value ("${raw}") ` +
        `— it can't be an RGB-triple mapping; use "passthrough" instead`,
    );
  }
  return hexToRgbTriple(raw);
}

/** Canonical raw value for a passthrough `token` in `theme`, verbatim. */
export function canonicalPassthrough(canonicalMap, token, theme) {
  if (!canonicalMap.has(token)) {
    throw new Error(
      `canonical token "--${token}" not found in the ${theme} block of ` +
        `${TOKENS_CSS_REL} — the consumer passthrough mapping is stale`,
    );
  }
  return canonicalMap.get(token).trim();
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

function renderBlock(consumer, canonical, theme, selector) {
  const canonicalMap = canonical[theme];
  const lines = [`${selector} {`];
  for (const [suffix, token] of consumer.mappings) {
    const triple = canonicalRgbTriple(canonicalMap, token, theme);
    const hex = rgbTripleToHex(triple).toUpperCase();
    lines.push(`  --color-${suffix}: ${triple}; /* ${hex} — --${token} */`);
  }
  for (const [varName, token] of consumer.passthrough) {
    const raw = canonicalPassthrough(canonicalMap, token, theme);
    lines.push(`  --${varName}: ${raw}; /* --${token} */`);
  }
  lines.push("}");
  return lines.join("\n");
}

/**
 * Render a consumer's complete generated CSS (header + light + dark blocks).
 *
 * The header names the generator and the canonical source so anyone who opens
 * the file — or reviews its diff — knows not to hand-edit it.
 */
export function renderConsumerCss(consumer, canonical) {
  if (consumer.mappings.length + consumer.passthrough.length === 0) {
    throw new Error(
      `consumer "${consumer.name}" has no mappings or passthrough entries — ` +
        `refusing to emit an empty token block`,
    );
  }
  const header = [
    "/*",
    " * GENERATED FILE — DO NOT EDIT.",
    " *",
    ` * Source:    ${TOKENS_CSS_REL}`,
    ` * Generator: node scripts/generate_foundry_tokens.mjs`,
    ` * Consumer:  ${consumer.name}`,
    " *",
    " * Each --color-* value is a space-separated RGB triple (\"R G B\", not",
    " * \"#rrggbb\") because tailwind.config.js consumes it as",
    " * `rgb(var(--color-*) / <alpha-value>)` — the only form from which",
    " * Tailwind can generate opacity-modified utilities (bg-foundry-primary/10).",
    " *",
    " * Edit the canonical source above and re-run the generator; CI's",
    " * token-drift job fails if this file and the source disagree (#5095).",
    " */",
  ].join("\n");

  return (
    [
      header,
      renderBlock(consumer, canonical, "light", consumer.lightSelector),
      renderBlock(consumer, canonical, "dark", consumer.darkSelector),
    ].join("\n\n") + "\n"
  );
}
