#!/usr/bin/env node
/**
 * Foundry token generator — emits each consumer's Tailwind-consumable
 * `--color-*: R G B` block from the canonical design-system source (#5095).
 *
 * Why: the generated output is CHECKED IN rather than produced at build time.
 * Three reasons, in order of weight: (1) a token change then shows up as a
 * reviewable colour diff in the PR that causes it, instead of silently
 * appearing in a deploy; (2) `crates/trusty-agents` publishes its UI from the
 * crate source tree and supports `SKIP_UI_BUILD=1`, so a build-time step would
 * have to run in a path that is explicitly allowed not to run; (3) the future
 * `website/` (#5092) deploys from its own subdirectory on Vercel, where
 * reaching up into `docs/` at build time is fragile. Checked-in output plus a
 * drift gate is also this repo's existing precedent (capabilities-drift.yml,
 * token-drift.yml).
 *
 * What: `--check` re-renders every consumer and compares to the file on disk,
 * exiting non-zero with a unified-style report on any difference. The default
 * (write) mode rewrites each consumer's `outFile`. Mapping tables and rendering
 * live in `scripts/lib/foundry-tokens.mjs`, shared with
 * `scripts/check_token_drift.mjs`.
 *
 * Test: `node --test scripts/generate_foundry_tokens.test.mjs`.
 */

import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import path from "node:path";

import {
  CONSUMERS,
  REPO_ROOT,
  TOKENS_CSS_REL,
  parseCanonical,
  renderConsumerCss,
} from "./lib/foundry-tokens.mjs";

/**
 * Render one consumer and report whether the on-disk file already matches.
 * Returns `{ name, outFile, expected, actual, matches }`; `actual` is null
 * when the file does not exist yet.
 */
export function renderConsumerState(consumer, canonical, repoRoot = REPO_ROOT) {
  const outPath = path.join(repoRoot, consumer.outFile);
  const expected = renderConsumerCss(consumer, canonical);
  const actual = existsSync(outPath) ? readFileSync(outPath, "utf8") : null;
  return {
    name: consumer.name,
    outFile: consumer.outFile,
    outPath,
    expected,
    actual,
    matches: actual === expected,
  };
}

/** First differing line between two texts, as a human-readable report. */
function firstDifference(expected, actual) {
  if (actual === null) return "  file does not exist";
  const exp = expected.split("\n");
  const act = actual.split("\n");
  for (let i = 0; i < Math.max(exp.length, act.length); i++) {
    if (exp[i] !== act[i]) {
      return (
        `  line ${i + 1}:\n` +
        `    expected: ${JSON.stringify(exp[i] ?? "<eof>")}\n` +
        `    found:    ${JSON.stringify(act[i] ?? "<eof>")}`
      );
    }
  }
  return "  files differ only in trailing bytes";
}

function main(argv) {
  const check = argv.includes("--check");
  const canonical = parseCanonical();

  console.log(`Canonical source: ${TOKENS_CSS_REL}`);
  console.log(check ? "Mode: --check (no writes)" : "Mode: write");
  console.log("");

  const stale = [];
  for (const consumer of CONSUMERS) {
    const state = renderConsumerState(consumer, canonical);
    if (check) {
      if (state.matches) {
        console.log(`OK    ${state.name} — ${state.outFile}`);
      } else {
        console.log(`STALE ${state.name} — ${state.outFile}`);
        stale.push(state);
      }
      continue;
    }
    if (state.matches) {
      console.log(`UNCHANGED ${state.name} — ${state.outFile}`);
      continue;
    }
    mkdirSync(path.dirname(state.outPath), { recursive: true });
    writeFileSync(state.outPath, state.expected, "utf8");
    console.log(`WROTE     ${state.name} — ${state.outFile}`);
  }

  if (CONSUMERS.length === 0) {
    console.error("FATAL: no consumers configured — refusing a vacuous run");
    process.exit(1);
  }

  if (stale.length > 0) {
    console.error("");
    console.error(
      `GENERATED TOKEN FILES ARE STALE (${stale.length} of ${CONSUMERS.length}):`,
    );
    for (const s of stale) {
      console.error(`  ${s.outFile}`);
      console.error(firstDifference(s.expected, s.actual));
    }
    console.error("");
    console.error("Fix: node scripts/generate_foundry_tokens.mjs");
    process.exit(1);
  }

  console.log("");
  console.log(
    check
      ? "All generated token files match the canonical source."
      : "Generation complete.",
  );
}

// Only run the CLI when executed directly, never on import from the test file.
const isMainModule = import.meta.url === `file://${process.argv[1]}`;
if (isMainModule) {
  main(process.argv.slice(2));
}

export { main, firstDifference };
