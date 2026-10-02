#!/usr/bin/env node
/**
 * Regression tests for scripts/roadmap/generate.mjs (docs/roadmap first PR).
 *
 * Why: `scripts/check_generated_regions.sh`'s `docs/roadmap/*.md)` owner case
 * (patched in this same PR) exists specifically so that a marked roadmap page
 * cannot ship without this file — a generator with no test would let
 * hand-maintained prose silently start looking machine-generated, the exact
 * failure mode `check_generated_regions.sh`'s header describes. This suite is
 * that owner: it proves the splice never touches hand-written text, that
 * regeneration is idempotent, that a milestone's internal notes below `---`
 * never reach the page, and that an untagged milestone is excluded.
 *
 * What: exercises the exported pure functions directly, against small inline
 * fixtures — never the real `gh api` or the real docs/roadmap page — so these
 * tests are independent of what GitHub's live milestones currently say.
 *
 * Test: `node --test scripts/roadmap/generate.test.mjs`.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";

import {
  beginLine,
  endLine,
  parseMilestoneRoadmap,
  displayTitle,
  renderRegion,
  spliceRegion,
  groupByStage,
  parseReleaseOrder,
  main,
} from "./generate.mjs";

/** A minimal GitHub milestone object, overridable per test. */
function milestoneFixture(overrides = {}) {
  return {
    number: 1,
    title: "trusty-mpm 9.9.9",
    description: "A public paragraph.\n\nRoadmap: trusty-mpm · next",
    open_issues: 3,
    closed_issues: 1,
    due_on: null,
    html_url: "https://github.com/bobmatnyc/trusty-tools/milestone/1",
    ...overrides,
  };
}

const HAND_WRITTEN_FILE = [
  "<!-- The block between BEGIN/END GENERATED is rebuilt by scripts/roadmap/generate.mjs. -->",
  "",
  "# trusty-mpm roadmap",
  "",
  "Hand-written intro paragraph, kept byte for byte across regeneration.",
  "",
  beginLine(),
  "STALE PLACEHOLDER REGION",
  endLine(),
  "",
  "Hand-written trailer paragraph, also kept byte for byte.",
  "",
].join("\n");

test("splice preserves the hand-written intro and trailer byte for byte", () => {
  const region = "## Next\n\n### 9.9.9\n\nA public paragraph.\n\n1 of 4 items done · [follow on GitHub](x)";
  const after = spliceRegion(HAND_WRITTEN_FILE, region);

  assert.ok(
    after.startsWith(
      "<!-- The block between BEGIN/END GENERATED is rebuilt by scripts/roadmap/generate.mjs. -->\n" +
        "\n# trusty-mpm roadmap\n\nHand-written intro paragraph, kept byte for byte across regeneration.\n\n" +
        beginLine(),
    ),
    "hand-written intro must survive regeneration unchanged",
  );
  assert.ok(
    after.endsWith("\nHand-written trailer paragraph, also kept byte for byte.\n"),
    "hand-written trailer must survive regeneration unchanged",
  );
  assert.ok(after.includes("### 9.9.9"), "the new region must be present");
  assert.ok(!after.includes("STALE PLACEHOLDER REGION"), "the old region must be gone");
});

test("regenerating twice from the same milestones produces no diff", () => {
  const milestones = [milestoneFixture()];
  const regionA = renderRegion(milestones, "trusty-mpm");
  const splicedOnce = spliceRegion(HAND_WRITTEN_FILE, regionA);

  const regionB = renderRegion(milestones, "trusty-mpm");
  const splicedTwice = spliceRegion(splicedOnce, regionB);

  assert.equal(splicedTwice, splicedOnce, "a second regeneration must be a no-op");
});

test("text below the --- divider never appears in the rendered output", () => {
  const milestone = milestoneFixture({
    description:
      "Public paragraph readers may see.\n\nRoadmap: trusty-mpm · now\n---\nINTERNAL SCOPE NOTE, NEVER PUBLISHED",
  });
  const region = renderRegion([milestone], "trusty-mpm");

  assert.ok(region.includes("Public paragraph readers may see."));
  assert.ok(!region.includes("INTERNAL SCOPE NOTE"));
  assert.ok(!region.includes("---"));
});

test("a milestone with no Roadmap: line is excluded", () => {
  const tagged = milestoneFixture({ number: 1, title: "trusty-mpm 9.9.9" });
  const untagged = milestoneFixture({
    number: 2,
    title: "trusty-mpm 8.8.8",
    description: "Plain prose with no machine-readable roadmap line at all.",
  });

  const region = renderRegion([tagged, untagged], "trusty-mpm");

  assert.ok(region.includes("9.9.9"));
  assert.ok(!region.includes("8.8.8"));
});

test("a milestone tagged for a different crate is excluded from this crate's page", () => {
  const other = milestoneFixture({
    title: "trusty-search 3.0.0",
    description: "Not ours.\n\nRoadmap: trusty-search · next",
  });
  const region = renderRegion([other], "trusty-mpm");
  assert.equal(region, "_No trusty-mpm milestone currently carries a `Roadmap:` line._");
});

test("displayTitle drops a leading crate-name prefix but leaves a bare version alone", () => {
  assert.equal(displayTitle("trusty-mpm 2.0.0", "trusty-mpm"), "2.0.0");
  assert.equal(displayTitle("1.7.2", "trusty-mpm"), "1.7.2");
});

test("parseMilestoneRoadmap rejects a malformed Roadmap line rather than guessing", () => {
  assert.equal(parseMilestoneRoadmap("Roadmap: trusty-mpm - now (wrong separator)"), null);
  assert.equal(parseMilestoneRoadmap("no roadmap line here at all"), null);
  assert.equal(parseMilestoneRoadmap(null), null);
});

test("spliceRegion throws rather than silently no-op when a marker is missing", () => {
  assert.throws(() => spliceRegion("# no markers here\n", "region"), /marker/);
  assert.throws(
    () => spliceRegion(`${endLine()}\n${beginLine()}\n`, "region"),
    /does not follow BEGIN/,
  );
});

test("main() end to end: fixture milestones splice cleanly and --check reports no drift", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "roadmap-gen-test-"));
  try {
    const fixturePath = path.join(dir, "milestones.json");
    const targetPath = path.join(dir, "trusty-mpm.md");
    writeFileSync(
      fixturePath,
      JSON.stringify([milestoneFixture({ title: "trusty-mpm 9.9.9" })]),
    );
    writeFileSync(targetPath, HAND_WRITTEN_FILE);

    main(["--crate", "trusty-mpm", "--file", targetPath, "--fixture", fixturePath]);
    const written = readFileSync(targetPath, "utf8");
    assert.ok(written.includes("### 9.9.9"));
    assert.ok(written.includes("Hand-written intro paragraph"));

    process.exitCode = undefined;
    main(["--crate", "trusty-mpm", "--file", targetPath, "--fixture", fixturePath, "--check"]);
    assert.equal(process.exitCode, undefined, "--check must report no drift once regenerated");
  } finally {
    process.exitCode = undefined;
    rmSync(dir, { recursive: true, force: true });
  }
});

// --- release-order sort key (#9085) ---------------------------------------

/** A `next`-stage milestone with an optional Release-order line. */
function ordered(number, key, extra = {}) {
  const line = key === undefined ? "" : `Release-order: ${key}\n`;
  return milestoneFixture({
    number,
    title: `trusty-mpm 0.0.${number}`,
    description: `Prose ${number}.\n${line}Roadmap: trusty-mpm · next`,
    ...extra,
  });
}

const numbersOf = (milestones) =>
  groupByStage(milestones, "trusty-mpm").next.map((m) => m.number);

/** Capture console.error output while `fn` runs. */
function captureStderr(fn) {
  const original = console.error;
  const lines = [];
  console.error = (...args) => lines.push(args.join(" "));
  try {
    return { result: fn(), lines };
  } finally {
    console.error = original;
  }
}

test("release-order: keyed milestones sort ascending regardless of number", () => {
  // Mirrors the real data: 2.1.0 (#72), 2.0.0 (#97), 1.9.0 (#124).
  assert.deepEqual(
    numbersOf([ordered(72, 3), ordered(97, 2), ordered(124, 1)]),
    [124, 97, 72],
  );
});

test("release-order: keyed milestones sort before unkeyed, unkeyed keep due/number order", () => {
  const unkeyedDated = ordered(5, undefined, { due_on: "2026-01-01T00:00:00Z" });
  const unkeyedLow = ordered(3, undefined);
  const unkeyedHigh = ordered(4, undefined);
  assert.deepEqual(numbersOf([unkeyedHigh, unkeyedLow, unkeyedDated, ordered(99, 7)]), [
    99, 5, 3, 4,
  ]);
});

test("release-order: equal keys fall back to due_on, then number", () => {
  const dated = ordered(50, 1, { due_on: "2026-01-01T00:00:00Z" });
  const undated = ordered(10, 1);
  const lowNumber = ordered(20, 1, { due_on: "2026-06-01T00:00:00Z" });
  const highNumber = ordered(30, 1, { due_on: "2026-06-01T00:00:00Z" });
  assert.deepEqual(numbersOf([highNumber, undated, lowNumber, dated]), [50, 20, 30, 10]);
});

test("release-order: parse tolerates whitespace and position, rejects anything but a non-negative integer", () => {
  assert.equal(parseReleaseOrder("Intro\n   Release-order:   12  \nmore"), 12);
  assert.equal(parseReleaseOrder("Release-order: 0"), 0);
  assert.equal(parseReleaseOrder("no key"), null);
  assert.equal(parseReleaseOrder(null), null);

  const { lines } = captureStderr(() => {
    for (const bad of ["soon", "-1", "2.5", "", "1e3", "99999999999999999999"]) {
      assert.equal(parseReleaseOrder(`Release-order: ${bad}`), null, `value ${bad}`);
    }
  });
  assert.equal(lines.length, 6, "each malformed value warns once on stderr");
  assert.ok(lines.every((l) => l.startsWith("warning:")));
});

test("release-order: a malformed key warns, counts as unkeyed, and does not crash", () => {
  const { result, lines } = captureStderr(() =>
    numbersOf([ordered(1, "soon"), ordered(2, 5), ordered(3, undefined)]),
  );
  assert.deepEqual(result, [2, 1, 3]);
  assert.equal(lines.length, 1);
  assert.ok(lines[0].includes("#1"), "the warning names the milestone");
});

test("release-order: the line is not published; unkeyed input keeps due/number order", () => {
  const region = renderRegion([ordered(2, 1)], "trusty-mpm");
  assert.ok(!region.includes("Release-order"));

  // Same milestones, unkeyed: today's due_on/number order. Keyed: reordered,
  // so this assertion fails against a comparator that ignores the key.
  const plain = [ordered(72, undefined), ordered(97, undefined), ordered(124, undefined)];
  assert.deepEqual(numbersOf(plain), [72, 97, 124]);
  const keyed = [ordered(72, 3), ordered(97, 2), ordered(124, 1)];
  assert.notDeepEqual(numbersOf(keyed), numbersOf(plain));
});
