#!/usr/bin/env node
/**
 * Per-crate roadmap page generator (docs/roadmap first PR).
 *
 * Why: `docs/roadmap/<crate>.md` carries a hand-written intro plus a
 * generated Now/Next/Later body, so the milestone counts and descriptions
 * stop drifting the way hand-maintained roadmap prose always does. Nothing
 * previously read GitHub milestones into a docs page — this script is that
 * bridge, following the zero-dependency `scripts/check_token_drift.mjs`
 * precedent (plain Node ESM, no dependency, exported pure functions for a
 * `node:test` regression suite, a `main()` guarded by an `isMainModule`
 * check so importing this file never has side effects).
 *
 * What: reads open milestones for a repo (live via `gh api --paginate`, or
 * from a JSON fixture file for tests and dry runs), selects the ones whose
 * `description` carries a line `Roadmap: <crate> · now|next|later` (case-
 * insensitive on the stage word, exact match on the crate), and publishes
 * only the text ABOVE a `---` divider line — a milestone's description can
 * carry internal scope notes below that divider, and those never reach the
 * page. The `Roadmap:` line itself is stripped from the published prose;
 * it is a machine directive, not copy. Selected milestones are grouped by
 * stage, sorted by due date then milestone number, and rendered as
 * `### <heading>` blocks with a progress line and a link back to GitHub.
 * The result REPLACES only the text between
 * `<!-- BEGIN GENERATED: roadmap -->` / `<!-- END GENERATED: roadmap -->`
 * markers (`scripts/check_generated_regions.sh`'s marker syntax) in the
 * target file; everything outside those markers — including a file that
 * does not exist yet, which this script does not create — is left alone.
 * Regenerating from the same milestone data twice produces byte-identical
 * output: the region body is a pure function of the milestone list, and
 * splicing never reads the region it is about to replace.
 *
 * A crate name that prefixes a milestone's title (`"trusty-mpm 2.0.0"` on
 * the `trusty-mpm` page) is dropped from the rendered heading, since the
 * page already names the crate; a title with no such prefix (`"1.7.2"`) is
 * left as-is.
 *
 * Test: scripts/roadmap/generate.test.mjs — splice-preserves-hand-written-
 * text, idempotency, below-the-divider text never appears in output, and a
 * milestone with no `Roadmap:` line is excluded.
 *
 * Usage:
 *   node scripts/roadmap/generate.mjs --crate trusty-mpm \
 *     --file docs/roadmap/trusty-mpm.md
 *   node scripts/roadmap/generate.mjs --crate trusty-mpm \
 *     --file docs/roadmap/trusty-mpm.md --fixture path/to/milestones.json
 *   node scripts/roadmap/generate.mjs --crate trusty-mpm \
 *     --file docs/roadmap/trusty-mpm.md --check   # exit 1 on drift, write nothing
 */

import { readFileSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(__dirname, "..", "..");

const REGION_ID = "roadmap";
const STAGE_ORDER = ["now", "next", "later"];
const STAGE_HEADING = { now: "Now", next: "Next", later: "Later" };

const DEFAULT_OWNER = "bobmatnyc";
const DEFAULT_REPO = "trusty-tools";

function beginLine(id = REGION_ID) {
  return `<!-- BEGIN GENERATED: ${id} -->`;
}

function endLine(id = REGION_ID) {
  return `<!-- END GENERATED: ${id} -->`;
}

/**
 * Extract the `{crate, stage, body}` a milestone's description publishes,
 * or `null` when it carries no `Roadmap:` line.
 *
 * `body` is everything above the `Roadmap:` line and above a `---` divider
 * (whichever comes first), trimmed of trailing blank lines — never the raw
 * description, which may continue past `---` into text that must not be
 * published (issue: internal scope notes read by reviewers, not readers).
 */
function parseMilestoneRoadmap(description) {
  const desc = typeof description === "string" ? description : "";
  const dividerMatch = desc.match(/^---\s*$/m);
  const publicPart = dividerMatch ? desc.slice(0, dividerMatch.index) : desc;

  const lines = publicPart.split("\n");
  const roadmapIdx = lines.findIndex((line) => /^Roadmap:\s*\S/.test(line.trim()));
  if (roadmapIdx === -1) {
    return null;
  }

  const match = lines[roadmapIdx]
    .trim()
    .match(/^Roadmap:\s*([A-Za-z0-9._-]+)\s*·\s*(now|next|later)\s*$/i);
  if (!match) {
    return null;
  }

  const bodyLines = lines.slice(0, roadmapIdx);
  while (bodyLines.length > 0 && bodyLines[bodyLines.length - 1].trim() === "") {
    bodyLines.pop();
  }

  return {
    crate: match[1],
    stage: match[2].toLowerCase(),
    body: bodyLines.join("\n").trim(),
  };
}

/** Drop a leading "<crate> " prefix from a milestone title; the page already names the crate. */
function displayTitle(title, crate) {
  const escaped = crate.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const prefix = new RegExp(`^${escaped}\\s+`, "i");
  const stripped = String(title).replace(prefix, "").trim();
  return stripped.length > 0 ? stripped : String(title);
}

function compareMilestones(a, b) {
  const dueA = a.due_on ? new Date(a.due_on).getTime() : Number.POSITIVE_INFINITY;
  const dueB = b.due_on ? new Date(b.due_on).getTime() : Number.POSITIVE_INFINITY;
  if (dueA !== dueB) {
    return dueA - dueB;
  }
  return (a.number ?? 0) - (b.number ?? 0);
}

/** Select the milestones for `crate` and bucket them by stage, sorted within each bucket. */
function groupByStage(milestones, crate) {
  const groups = { now: [], next: [], later: [] };
  for (const milestone of milestones) {
    const meta = parseMilestoneRoadmap(milestone.description);
    if (!meta || meta.crate.toLowerCase() !== crate.toLowerCase()) {
      continue;
    }
    groups[meta.stage].push({ ...milestone, roadmapBody: meta.body });
  }
  for (const stage of STAGE_ORDER) {
    groups[stage].sort(compareMilestones);
  }
  return groups;
}

function renderMilestoneBlock(milestone, crate) {
  const heading = displayTitle(milestone.title, crate);
  const total = (milestone.open_issues ?? 0) + (milestone.closed_issues ?? 0);
  const progress = `${milestone.closed_issues ?? 0} of ${total} items done · [follow on GitHub](${milestone.html_url})`;
  return `### ${heading}\n\n${milestone.roadmapBody}\n\n${progress}`;
}

/** Render the full generated-region body (no markers) for `crate` from `milestones`. */
function renderRegion(milestones, crate) {
  const groups = groupByStage(milestones, crate);
  const sections = STAGE_ORDER.filter((stage) => groups[stage].length > 0).map((stage) => {
    const blocks = groups[stage].map((m) => renderMilestoneBlock(m, crate));
    return `## ${STAGE_HEADING[stage]}\n\n${blocks.join("\n\n")}`;
  });

  if (sections.length === 0) {
    return `_No ${crate} milestone currently carries a \`Roadmap:\` line._`;
  }
  return sections.join("\n\n");
}

/**
 * Replace the text between the BEGIN/END marker lines in `source` with
 * `regionBody`. Text before and including BEGIN, and from END onward, is
 * returned unchanged (byte-for-byte, since this operates on the exact
 * lines of `source`). Throws when the markers are missing or out of order.
 */
function spliceRegion(source, regionBody, id = REGION_ID) {
  const begin = beginLine(id);
  const end = endLine(id);
  const lines = source.split("\n");
  const beginIdx = lines.findIndex((line) => line === begin);
  const endIdx = lines.findIndex((line) => line === end);

  if (beginIdx === -1 || endIdx === -1) {
    throw new Error(
      `spliceRegion: marker(s) for id "${id}" not found (begin=${beginIdx}, end=${endIdx})`,
    );
  }
  if (endIdx <= beginIdx) {
    throw new Error(`spliceRegion: END marker for id "${id}" does not follow BEGIN`);
  }

  const before = lines.slice(0, beginIdx + 1);
  const after = lines.slice(endIdx);
  const regionLines = regionBody.split("\n");
  return [...before, "", ...regionLines, "", ...after].join("\n");
}

/** Load milestones from a JSON fixture file, or live via `gh api --paginate`. */
function loadMilestones({ fixture, owner = DEFAULT_OWNER, repo = DEFAULT_REPO }) {
  if (fixture) {
    const raw = readFileSync(path.resolve(process.cwd(), fixture), "utf8");
    return JSON.parse(raw);
  }
  const stdout = execFileSync(
    "gh",
    [
      "api",
      `repos/${owner}/${repo}/milestones`,
      "--paginate",
      "-X",
      "GET",
      "-F",
      "per_page=100",
    ],
    { encoding: "utf8", maxBuffer: 32 * 1024 * 1024 },
  );
  return JSON.parse(stdout);
}

function parseArgs(argv) {
  const args = { owner: DEFAULT_OWNER, repo: DEFAULT_REPO, check: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === "--crate") args.crate = argv[++i];
    else if (arg === "--file") args.file = argv[++i];
    else if (arg === "--fixture") args.fixture = argv[++i];
    else if (arg === "--owner") args.owner = argv[++i];
    else if (arg === "--repo") args.repo = argv[++i];
    else if (arg === "--check") args.check = true;
    else if (arg === "--help") args.help = true;
  }
  return args;
}

function printUsage() {
  console.log(
    [
      "Usage: node scripts/roadmap/generate.mjs --crate <name> --file <path> [--fixture <path>] [--check]",
      "",
      "  --crate <name>     Crate to select via the milestone's `Roadmap: <crate> · <stage>` line (required)",
      "  --file <path>      Repo-relative target markdown file carrying the BEGIN/END markers (required)",
      "  --fixture <path>   Read milestones from this JSON file instead of `gh api` (tests, dry runs)",
      "  --owner/--repo     Override the GitHub owner/repo (default bobmatnyc/trusty-tools)",
      "  --check            Fail with exit 1 on drift instead of writing; writes nothing",
    ].join("\n"),
  );
}

function main(argv) {
  const args = parseArgs(argv);
  if (args.help || !args.crate || !args.file) {
    printUsage();
    process.exitCode = args.help ? 0 : 2;
    return;
  }

  const milestones = loadMilestones(args);
  const region = renderRegion(milestones, args.crate);
  const filePath = path.resolve(REPO_ROOT, args.file);
  const before = readFileSync(filePath, "utf8");
  const after = spliceRegion(before, region);

  if (args.check) {
    if (after !== before) {
      console.error(`DRIFT: ${args.file} does not match the live-generated region.`);
      process.exitCode = 1;
      return;
    }
    console.log(`OK: ${args.file} matches the live-generated region.`);
    return;
  }

  if (after === before) {
    console.log(`OK: ${args.file} already up to date (no diff).`);
    return;
  }

  writeFileSync(filePath, after);
  console.log(`Wrote ${args.file} (region regenerated for crate "${args.crate}").`);
}

// Only run the CLI when executed directly, not when imported by
// scripts/roadmap/generate.test.mjs — an import must not have the side
// effect of shelling out to `gh` or writing a file.
const isMainModule = import.meta.url === `file://${process.argv[1]}`;
if (isMainModule) {
  main(process.argv.slice(2));
}

export {
  REPO_ROOT,
  REGION_ID,
  STAGE_ORDER,
  beginLine,
  endLine,
  parseMilestoneRoadmap,
  displayTitle,
  compareMilestones,
  groupByStage,
  renderMilestoneBlock,
  renderRegion,
  spliceRegion,
  loadMilestones,
  parseArgs,
  main,
};
