#!/usr/bin/env node
/**
 * Regression tests for check_token_drift.mjs (issue #3486 review follow-up).
 *
 * Why: the manual negative-test performed while authoring the script
 * (mutate a value, confirm failure, revert) was run once, by hand, locally
 * — CI never re-runs it, so a future refactor of the comparison/guard logic
 * could reintroduce a false-green with nothing to catch it. Concretely: a
 * code-critic review of this script found that emptying an ENFORCED
 * crate's `mappings`/`passthrough` to `[]` made the script print
 * "OK ... matches canonical" and exit 0 even with a simultaneously wrong
 * `app.css` value — the exact false-green this whole check exists to
 * prevent. This file mechanizes that finding (and the general drift/parse-
 * failure paths) as an executable, CI-run regression suite using only
 * Node's built-in test runner (`node:test` + `node:assert/strict`) — no new
 * dependency.
 *
 * What: uses `node scripts/check_token_drift.mjs`'s exported pure helpers
 * (`checkCrate`, `parseCanonicalFromSource`, `parseCrate`, `ENFORCED`)
 * against small inline CSS fixtures (and one on-disk temp fixture for the
 * missing-block case, since that path is disk-based) rather than the real
 * tokens.css / app.css files, so these tests are independent of whatever
 * the real design tokens currently say.
 *
 * Test: `node --test scripts/check_token_drift.test.mjs` (see the
 * `token-drift` CI job, which runs this after the drift check itself).
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import {
  ENFORCED,
  checkCrate,
  parseCanonical,
  parseCanonicalFromSource,
  parseCrate,
  TOKENS_CSS_PATH,
} from "./check_token_drift.mjs";

// A minimal canonical source with one token that differs between light and
// dark, used by the in-memory (no-disk) fixture tests below.
const CANONICAL_SOURCE = `
:root {
  --trusty-foo: #112233;
}

[data-theme='dark'], .dark {
  --trusty-foo: #445566;
}
`;

function makeFixtureCrate(overrides = {}) {
  return {
    name: "fixture-crate",
    file: "unused-when-parsedOverride-is-supplied.css",
    mode: "rgb-triple",
    lightSelector: /:root\s*\{([\s\S]*?)\n\}/,
    darkSelector: /\.dark\s*\{([\s\S]*?)\n\}/,
    mappings: [["foo", "trusty-foo"]],
    passthrough: [],
    ...overrides,
  };
}

// A canonical source for the hex-mode fixtures: two hex tokens plus one
// non-hex (rgba) token that hex mode must SKIP, in each theme block.
const HEX_CANONICAL_SOURCE = `
:root {
  --trusty-bg: #f0f0f0;
  --trusty-text: #101010;
  --trusty-hover: rgba(1, 2, 3, 0.5);
}

[data-theme='dark'], .dark {
  --trusty-bg: #202020;
  --trusty-text: #e0e0e0;
  --trusty-hover: rgba(9, 8, 7, 0.5);
}
`;

function makeHexFixtureCrate(overrides = {}) {
  return {
    name: "hex-fixture-crate",
    file: "unused-when-parsedOverride-is-supplied.css",
    mode: "hex",
    lightSelector: /:root\s*\{([\s\S]*?)\n\}/,
    darkSelector: /\[data-theme=(['"])dark\1\]\s*\{([\s\S]*?)\n\}/,
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// (a) drift on a mutated value is detected — non-zero (throws-free; the
//     caller inspects the returned diffs and CLI is the one that exits 1).
// ---------------------------------------------------------------------------

test("checkCrate detects a mutated dark-theme value as drift", () => {
  const canonical = parseCanonicalFromSource(CANONICAL_SOURCE, "<inline canonical fixture>");

  const parsedCrate = {
    light: new Map([["color-foo", "17 34 51"]]), // correct: #112233
    dark: new Map([["color-foo", "1 2 3"]]), // WRONG: should be 68 85 102 (#445566)
  };

  const diffs = checkCrate(makeFixtureCrate(), canonical, parsedCrate);

  assert.equal(diffs.length, 1, "expected exactly one drift (dark theme only)");
  assert.equal(diffs[0].theme, "dark");
  assert.equal(diffs[0].var, "--color-foo");
  assert.match(diffs[0].actual, /^1 2 3/);
  assert.match(diffs[0].expected, /^68 85 102/);
});

test("checkCrate reports zero diffs when every value matches canonical", () => {
  const canonical = parseCanonicalFromSource(CANONICAL_SOURCE, "<inline canonical fixture>");

  const parsedCrate = {
    light: new Map([["color-foo", "17 34 51"]]), // #112233
    dark: new Map([["color-foo", "68 85 102"]]), // #445566
  };

  const diffs = checkCrate(makeFixtureCrate(), canonical, parsedCrate);
  assert.equal(diffs.length, 0);
});

// ---------------------------------------------------------------------------
// (b) a missing :root/dark block throws (disk-based — extractBlock's
//     failure path is exercised against a real temp file).
// ---------------------------------------------------------------------------

test("parseCrate throws when the dark block is missing from the file", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "token-drift-test-"));
  const filePath = path.join(dir, "app.css");
  try {
    // Valid :root block, but no `.dark { ... }` block at all.
    writeFileSync(
      filePath,
      `:root {\n  --color-foo: 17 34 51;\n}\n`,
      "utf8",
    );

    const crate = makeFixtureCrate({ file: filePath });
    assert.throws(
      () => parseCrate(crate),
      /could not find dark block/,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// ---------------------------------------------------------------------------
// (c) a missing canonical token throws (crate mapping references a token
//     that doesn't exist in the canonical source).
// ---------------------------------------------------------------------------

test("checkCrate throws when a mapped canonical token doesn't exist", () => {
  const canonical = parseCanonicalFromSource(CANONICAL_SOURCE, "<inline canonical fixture>");

  const parsedCrate = {
    light: new Map([["color-bar", "1 2 3"]]),
    dark: new Map([["color-bar", "1 2 3"]]),
  };

  const crate = makeFixtureCrate({
    mappings: [["bar", "trusty-does-not-exist"]],
  });

  assert.throws(
    () => checkCrate(crate, canonical, parsedCrate),
    /canonical token "--trusty-does-not-exist" not found/,
  );
});

// ---------------------------------------------------------------------------
// (d) the zero-comparison guard — both as a data invariant over the real
//     ENFORCED table (protects the #3487-#3490 migration PRs from silently
//     emptying an entry) and as a direct exercise of checkCrate's own throw.
// ---------------------------------------------------------------------------

test("every ENFORCED crate compares >0 real tokens against canonical", () => {
  // Covers all 7 UI crates (epic #3486): trusty-agents, trusty-code-gui,
  // trusty-mpm-gui (rgb-triple) + trusty-console, trusty-analyze,
  // trusty-search, trusty-memory (hex). Running the REAL comparison against
  // the REAL canonical file is the strongest form of "at least one token is
  // compared" — checkCrate throws if a crate performs zero comparisons (a
  // mis-configured path/selector), so a bare doesNotThrow here would already
  // catch that; we additionally assert the returned comparedCount is > 0 and
  // that no crate has drifted, making this the CI regression for the flip.
  assert.ok(ENFORCED.length === 7, `expected 7 enforced crates, got ${ENFORCED.length}`);
  const canonical = parseCanonical(TOKENS_CSS_PATH);
  for (const crate of ENFORCED) {
    let diffs;
    assert.doesNotThrow(() => {
      diffs = checkCrate(crate, canonical);
    }, `${crate.name}: checkCrate threw (mis-configured path/selector?)`);
    assert.ok(
      diffs.comparedCount > 0,
      `${crate.name}: compared zero tokens — an enforced crate must compare ≥1`,
    );
    assert.equal(
      diffs.length,
      0,
      `${crate.name}: ${diffs.length} token(s) drifted from canonical`,
    );
  }
});

test("ALLOWLIST is empty — no UI crate is exempt from enforcement", async () => {
  const { ALLOWLIST } = await import("./check_token_drift.mjs");
  assert.equal(ALLOWLIST.length, 0);
});

test("checkCrate refuses to report a pass with zero comparisons configured", () => {
  const canonical = parseCanonicalFromSource(CANONICAL_SOURCE, "<inline canonical fixture>");
  const crate = makeFixtureCrate({ mappings: [], passthrough: [] });

  // Even with a bogus, never-read file path and no parsedOverride, the
  // guard must fire before any file I/O is attempted.
  assert.throws(
    () => checkCrate(crate, canonical),
    /mappings\+passthrough is empty/,
  );
});

// ---------------------------------------------------------------------------
// (e) plain-CSS-hex comparison mode (mode: "hex", used by trusty-console,
//     trusty-analyze, trusty-search, trusty-memory).
// ---------------------------------------------------------------------------

test("hex mode: matching hex values report zero diffs (intersection + skips)", () => {
  const canonical = parseCanonicalFromSource(HEX_CANONICAL_SOURCE, "<inline hex canonical>");

  const parsedCrate = {
    // Correct hex for the two canonical hex tokens; a crate-only extension
    // token (--trusty-extra) that must be IGNORED; and --trusty-hover which
    // is non-hex in canonical, so it must be SKIPPED (not compared) even
    // though the crate defines it.
    light: new Map([
      ["trusty-bg", "#F0F0F0"], // case-insensitive match of #f0f0f0
      ["trusty-text", "#101010"],
      ["trusty-hover", "rgba(1, 2, 3, 0.5)"],
      ["trusty-extra", "#abcdef"],
    ]),
    dark: new Map([
      ["trusty-bg", "#202020"],
      ["trusty-text", "#e0e0e0"],
    ]),
  };

  const diffs = checkCrate(makeHexFixtureCrate(), canonical, parsedCrate);
  assert.equal(diffs.length, 0, "expected no drift");
  // 2 hex tokens in light + 2 in dark = 4; hover skipped, extra ignored.
  assert.equal(diffs.comparedCount, 4);
});

test("hex mode: a mutated hex value is detected as drift", () => {
  const canonical = parseCanonicalFromSource(HEX_CANONICAL_SOURCE, "<inline hex canonical>");

  const parsedCrate = {
    light: new Map([
      ["trusty-bg", "#f0f0f0"],
      ["trusty-text", "#101010"],
    ]),
    dark: new Map([
      ["trusty-bg", "#999999"], // WRONG: canonical dark is #202020
      ["trusty-text", "#e0e0e0"],
    ]),
  };

  const diffs = checkCrate(makeHexFixtureCrate(), canonical, parsedCrate);
  assert.equal(diffs.length, 1, "expected exactly one drift (dark bg)");
  assert.equal(diffs[0].theme, "dark");
  assert.equal(diffs[0].var, "--trusty-bg");
  assert.equal(diffs[0].expected, "#202020");
  assert.equal(diffs[0].actual, "#999999");
});

test("hex mode: throws when the crate defines no canonical tokens (zero comparisons)", () => {
  const canonical = parseCanonicalFromSource(HEX_CANONICAL_SOURCE, "<inline hex canonical>");

  const parsedCrate = {
    light: new Map([["trusty-only-mine", "#123456"]]),
    dark: new Map([["trusty-only-mine", "#123456"]]),
  };

  assert.throws(
    () => checkCrate(makeHexFixtureCrate(), canonical, parsedCrate),
    /zero token comparisons performed \(hex mode\)/,
  );
});

test("hex mode: a missing dark block throws (disk-based)", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "token-drift-hex-test-"));
  const filePath = path.join(dir, "tokens.css");
  try {
    // Valid light block, but no `[data-theme='dark']` block at all.
    writeFileSync(
      filePath,
      `:root {\n  --trusty-bg: #f0f0f0;\n}\n`,
      "utf8",
    );

    const crate = makeHexFixtureCrate({ file: filePath });
    assert.throws(
      () => parseCrate(crate),
      /could not find dark block/,
    );
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
