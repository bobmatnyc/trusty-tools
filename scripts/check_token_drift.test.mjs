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
  parseCanonicalFromSource,
  parseCrate,
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
    lightSelector: /:root\s*\{([\s\S]*?)\n\}/,
    darkSelector: /\.dark\s*\{([\s\S]*?)\n\}/,
    mappings: [["foo", "trusty-foo"]],
    passthrough: [],
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

test("every ENFORCED crate has at least one mapping or passthrough entry", () => {
  assert.ok(ENFORCED.length > 0, "ENFORCED must not itself be empty");
  for (const crate of ENFORCED) {
    const tokenCount = crate.mappings.length + crate.passthrough.length;
    assert.ok(
      tokenCount > 0,
      `${crate.name}: mappings+passthrough is empty — an enforced crate ` +
        `must compare at least one token`,
    );
  }
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
