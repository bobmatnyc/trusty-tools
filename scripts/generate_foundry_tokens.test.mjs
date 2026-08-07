#!/usr/bin/env node
/**
 * Regression tests for the Foundry token generator (issue #5095).
 *
 * Why: the generator is the only thing standing between the canonical design
 * tokens and what the UI actually renders, and its `--check` mode is a CI
 * gate. Both failure modes are silent-by-default: a renderer that emits the
 * dark block first would invert the theme on `<html class="dark">` (`:root`
 * and `.dark` have EQUAL specificity, so source order alone decides), and a
 * `--check` that reports a match on a stale file would let a token change
 * merge without reaching the consumer. Neither shows up as a crash.
 *
 * What: exercises the pure renderer against inline canonical fixtures, plus
 * one test that re-renders the REAL trusty-agents profile and asserts it
 * equals the checked-in artifact — so a hand-edit to the generated file fails
 * here as well as in CI.
 *
 * Test: `node --test scripts/generate_foundry_tokens.test.mjs` (run by the
 * token-drift CI job before the check itself).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";

import {
  CONSUMERS,
  REPO_ROOT,
  blockRegexFor,
  consumerByName,
  hexToRgbTriple,
  parseCanonical,
  parseCanonicalFromSource,
  renderConsumerCss,
} from "./lib/foundry-tokens.mjs";
import { renderConsumerState } from "./generate_foundry_tokens.mjs";

const CANONICAL_SOURCE = `
:root {
  --trusty-accent: #b7410e;
  --trusty-surface-hover: rgba(183, 65, 14, 0.06);
  --trusty-font: 'IBM Plex Sans', sans-serif;
}

[data-theme='dark'], .dark {
  --trusty-accent: #d97742;
  --trusty-surface-hover: rgba(217, 119, 66, 0.09);
  --trusty-font: 'IBM Plex Sans', sans-serif;
}
`;

const FIXTURE_CANONICAL = parseCanonicalFromSource(CANONICAL_SOURCE, "fixture");

function fixtureConsumer(overrides = {}) {
  return {
    name: "fixture",
    outFile: "unused/fixture.css",
    lightSelector: ":root",
    darkSelector: ".dark",
    mappings: [["primary", "trusty-accent"]],
    passthrough: [["trusty-surface-hover", "trusty-surface-hover"]],
    ...overrides,
  };
}

test("renders hex as a space-separated RGB triple per theme", () => {
  const css = renderConsumerCss(fixtureConsumer(), FIXTURE_CANONICAL);
  assert.match(css, /:root \{[\s\S]*--color-primary: 183 65 14;/);
  assert.match(css, /\.dark \{[\s\S]*--color-primary: 217 119 66;/);
});

test("emits the light block BEFORE the dark block", () => {
  // :root and .dark are both specificity (0,1,0); on <html class="dark"> the
  // LATER rule wins. Emitting dark first would invert the whole theme.
  const css = renderConsumerCss(fixtureConsumer(), FIXTURE_CANONICAL);
  assert.ok(css.indexOf(":root {") < css.indexOf(".dark {"));
});

test("carries passthrough values over verbatim, not as a triple", () => {
  const css = renderConsumerCss(fixtureConsumer(), FIXTURE_CANONICAL);
  assert.match(css, /--trusty-surface-hover: rgba\(183, 65, 14, 0\.06\);/);
  assert.match(css, /--trusty-surface-hover: rgba\(217, 119, 66, 0\.09\);/);
});

test("marks the output as generated so it is not hand-edited", () => {
  const css = renderConsumerCss(fixtureConsumer(), FIXTURE_CANONICAL);
  assert.match(css, /GENERATED FILE — DO NOT EDIT/);
  assert.match(css, /scripts\/generate_foundry_tokens\.mjs/);
});

test("throws when a mapped canonical token no longer exists", () => {
  const consumer = fixtureConsumer({ mappings: [["primary", "trusty-gone"]] });
  assert.throws(
    () => renderConsumerCss(consumer, FIXTURE_CANONICAL),
    /--trusty-gone.*not found/s,
  );
});

test("throws when a mapped canonical token is not hex (needs passthrough)", () => {
  const consumer = fixtureConsumer({
    mappings: [["hover", "trusty-surface-hover"]],
  });
  assert.throws(
    () => renderConsumerCss(consumer, FIXTURE_CANONICAL),
    /not a hex value.*use "passthrough" instead/s,
  );
});

test("refuses to emit a consumer with no mappings at all", () => {
  // A profile emptied by accident would otherwise render two valid-looking but
  // token-free blocks and pass --check forever.
  const consumer = fixtureConsumer({ mappings: [], passthrough: [] });
  assert.throws(() => renderConsumerCss(consumer, FIXTURE_CANONICAL), /empty token block/);
});

test("consumer selectors survive the round-trip into a block regex", () => {
  const css = renderConsumerCss(fixtureConsumer(), FIXTURE_CANONICAL);
  for (const selector of [":root", ".dark"]) {
    const match = blockRegexFor(selector).exec(css);
    assert.ok(match, `blockRegexFor("${selector}") matched nothing`);
    assert.match(match[match.length - 1], /--color-primary:/);
  }
  // `.` must be escaped — an unescaped `.dark` would also match `:root {`'s
  // preceding character and silently pick the wrong block.
  assert.ok(!blockRegexFor(".dark").test(":root {\n  --x: 1;\n}"));
});

test("hexToRgbTriple rejects anything that is not a 6-digit hex", () => {
  assert.equal(hexToRgbTriple("#B7410E"), "183 65 14");
  assert.throws(() => hexToRgbTriple("#fff"), /not a 6-digit hex color/);
  assert.throws(() => hexToRgbTriple("rgba(1,2,3,.5)"), /not a 6-digit hex color/);
});

test("consumerByName throws on an unknown consumer", () => {
  assert.throws(() => consumerByName("nope"), /unknown consumer "nope"/);
  assert.equal(consumerByName("trusty-agents").name, "trusty-agents");
});

test("every configured consumer's checked-in file is up to date", () => {
  // The CI gate in executable form: a hand-edit to any generated file, or a
  // token change committed without regenerating, fails right here.
  assert.ok(CONSUMERS.length > 0, "no consumers configured");
  const canonical = parseCanonical();
  for (const consumer of CONSUMERS) {
    const state = renderConsumerState(consumer, canonical);
    assert.equal(
      state.actual,
      state.expected,
      `${consumer.outFile} is stale — run: node scripts/generate_foundry_tokens.mjs`,
    );
  }
});

test("renderConsumerState reports a mismatch instead of a false match", () => {
  const consumer = fixtureConsumer({ outFile: "does/not/exist.css" });
  const state = renderConsumerState(consumer, FIXTURE_CANONICAL);
  assert.equal(state.actual, null);
  assert.equal(state.matches, false);
});

test("the trusty-agents consumer still feeds tailwind.config.js's colors", () => {
  // Every --color-* the Tailwind config reads must be one the generator emits;
  // a renamed mapping key would otherwise leave `rgb(var(--color-x) / …)`
  // pointing at nothing and silently render transparent.
  const consumer = consumerByName("trusty-agents");
  const configPath = path.join(
    REPO_ROOT,
    "crates/trusty-agents/ui/tailwind.config.js",
  );
  const config = readFileSync(configPath, "utf8");
  const referenced = new Set(
    [...config.matchAll(/var\(--color-([a-z0-9-]+)\)/g)].map((m) => m[1]),
  );
  assert.ok(referenced.size > 0, "tailwind.config.js references no --color-* var");
  const emitted = new Set(consumer.mappings.map(([suffix]) => suffix));
  for (const name of referenced) {
    assert.ok(
      emitted.has(name),
      `tailwind.config.js reads --color-${name}, which no mapping emits`,
    );
  }
});
