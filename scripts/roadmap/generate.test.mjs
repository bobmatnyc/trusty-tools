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
