#!/usr/bin/env node
/**
 * trusty-tools roadmap generator: one page, every crate's open milestones.
 *
 * Why: `docs/roadmap/trusty-tools.md` is the single roadmap for the whole
 * workspace (owner instruction 2026-10-02, amending ruling 5a). It carries a
 * hand-written intro and release plan plus a generated per-crate body, so
 * milestone counts and descriptions stop drifting the way hand-maintained
 * roadmap prose always does. The website's `/docs/roadmap` page renders the
 * same file through `docs/public-manifest.tsv`, so there is no second copy.
 * Follows the zero-dependency `scripts/check_token_drift.mjs` precedent: plain
 * Node ESM, exported pure functions for a `node:test` suite, and a `main()`
 * guarded by an `isMainModule` check so an import has no side effects.
 *
 * What: reads the repo's OPEN milestones (live via `gh api --paginate`, or a
 * JSON fixture for tests and dry runs; a fixture's `state: "closed"` rows are
 * dropped too) and assigns each to a crate, first match wins:
 *   1. a title that starts with a crate name — `trusty-search 0.54.5 · bugfix`,
 *      `trusty agents mvp` (space form) — names that crate;
 *   2. otherwise the crate in the description's `Roadmap: <crate> · <stage>`;
 *   3. otherwise the title's last ` · <area>` segment through `AREA_CRATES`
 *      (`Backlog · mpm/core` -> trusty-mpm; a trailing `(…)` is ignored);
 *   4. otherwise no crate: the milestone is listed under "Across the workspace".
 *
 * A milestone whose description carries a `Roadmap: <crate> · now|next|later`
 * line publishes its prose: only the text ABOVE the `Roadmap:` line and above
 * a `---` divider, so internal scope notes below the divider never reach the
 * page. Every other open milestone is listed as a one-line link with its
 * progress and no prose, because its description was never written for
 * readers.
 *
 * Ordering is total, so the output is a pure function of the milestone list.
 * Within a crate: Roadmap milestones by stage (now, next, later), then by
 * release order, due date, milestone number; the untagged list by release
 * order, due date, number. Crates: those carrying a `Release-order:` key
 * first, by their lowest key, then the rest by name; "Across the workspace"
 * comes last.
 *
 * Release order (#9085, owner ruling 23): a description may carry a line
 * `Release-order: <non-negative integer>` anywhere, surrounding whitespace
 * tolerated. Keyed milestones sort first, ascending; unkeyed ones follow by
 * due date then number. A malformed value (`soon`, `-1`, `2.5`, empty) counts
 * as no key and warns on stderr; the generator never aborts on it. The line is
 * a machine directive and is stripped from published prose, like `Roadmap:`.
 *
 * The result REPLACES only the text between `<!-- BEGIN GENERATED: roadmap -->`
 * and `<!-- END GENERATED: roadmap -->` (`scripts/check_generated_regions.sh`'s
 * marker syntax). Everything outside the markers is left alone, and a missing
 * target file is an error, never created. Splicing never reads the region it
 * replaces, so regenerating from the same milestones is byte-identical.
 *
 * A crate name that prefixes a milestone's title is dropped from the rendered
 * heading, since the crate section already names it (`trusty-mpm 2.0.0` ->
 * `2.0.0`); any other title is left as-is.
 *
 * Test: scripts/roadmap/generate.test.mjs — splice preserves hand-written
 * text, idempotency, below-the-divider text never appears, a milestone with no
 * `Roadmap:` line publishes no prose, release-order ordering, and the
 * multi-crate assignment and grouping (`crate assignment` / `grouping` tests).
 *
 * Usage:
 *   node scripts/roadmap/generate.mjs --file docs/roadmap/trusty-tools.md
 *   node scripts/roadmap/generate.mjs --file docs/roadmap/trusty-tools.md \
 *     --fixture path/to/milestones.json
 *   node scripts/roadmap/generate.mjs --file docs/roadmap/trusty-tools.md \
 *     --check   # exit 1 on drift, write nothing
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
const WORKSPACE_HEADING = "Across the workspace";

const DEFAULT_OWNER = "bobmatnyc";
const DEFAULT_REPO = "trusty-tools";

/**
 * Area words used in `<kind> · <area>` milestone titles, mapped to the crate
 * they name. An area missing here (`analyze/review` spans two crates,
 * `optimize` spans all) leaves the milestone under "Across the workspace".
 */
const AREA_CRATES = {
  agents: "trusty-agents",
  audit: "trusty-audit",
  code: "trusty-code",
  console: "trusty-console",
  embedderd: "trusty-embedderd",
  installer: "trusty-installer",
  mcp: "trusty-mcp",
  memory: "trusty-memory",
  "mpm/core": "trusty-mpm",
  search: "trusty-search",
  "tc-services": "tc-services",
  tga: "trusty-git-analytics",
};

const TITLE_CRATE = /^trusty[- ]([a-z0-9]+(?:-[a-z0-9]+)*)(?=[\s:]|$)/i;

function beginLine(id = REGION_ID) {
  return `<!-- BEGIN GENERATED: ${id} -->`;
}

function endLine(id = REGION_ID) {
  return `<!-- END GENERATED: ${id} -->`;
}

const RELEASE_ORDER_LINE = /^\s*Release-order:(.*)$/im;

/**
 * Parse a milestone description's `Release-order:` key (#9085).
 *
 * Returns the non-negative integer, or `null` when the line is absent or its
 * value is malformed; a malformed value also warns on stderr (`label` names
 * the milestone) and never throws.
 */
function parseReleaseOrder(description, label = "milestone") {
  const desc = typeof description === "string" ? description : "";
  const match = desc.match(RELEASE_ORDER_LINE);
  if (!match) {
    return null;
  }
  const raw = match[1].trim();
  const value = /^\d+$/.test(raw) ? Number(raw) : Number.NaN;
  if (!Number.isSafeInteger(value)) {
    console.error(
      `warning: ${label}: ignoring malformed Release-order value ${JSON.stringify(raw)} (expected a non-negative integer)`,
    );
    return null;
  }
  return value;
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

  const bodyLines = lines
    .slice(0, roadmapIdx)
    .filter((line) => !RELEASE_ORDER_LINE.test(line));
  while (bodyLines.length > 0 && bodyLines[bodyLines.length - 1].trim() === "") {
    bodyLines.pop();
  }

  return {
    crate: match[1],
    stage: match[2].toLowerCase(),
    body: bodyLines.join("\n").trim(),
  };
}

/**
 * The crate a milestone belongs to, or `null` when none can be named. Order:
 * title prefix, then the `Roadmap:` line's crate, then the ` · <area>` title
 * suffix through `AREA_CRATES`. See the file header for why that order.
 */
function crateOf(milestone) {
  const title = String(milestone.title ?? "").trim();
  const prefix = title.match(TITLE_CRATE);
  if (prefix) {
    return `trusty-${prefix[1].toLowerCase()}`;
  }
  const meta = parseMilestoneRoadmap(milestone.description);
  if (meta) {
    return meta.crate.toLowerCase();
  }
  const segments = title.split(" · ");
  if (segments.length > 1) {
    const area = segments[segments.length - 1]
      .replace(/\s*\([^)]*\)\s*$/, "")
      .trim()
      .toLowerCase();
    return AREA_CRATES[area] ?? null;
  }
  return null;
}

/** Drop a leading "<crate> " prefix from a milestone title; the section already names the crate. */
function displayTitle(title, crate) {
  if (!crate) {
    return String(title);
  }
  const escaped = crate.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const prefix = new RegExp(`^${escaped}\\s+`, "i");
  const stripped = String(title).replace(prefix, "").trim();
  return stripped.length > 0 ? stripped : String(title);
}

/** Order by `releaseOrder` (keyed first, ascending), then due date, then number. */
function compareMilestones(a, b) {
  const keyA = a.releaseOrder ?? null;
  const keyB = b.releaseOrder ?? null;
  if (keyA !== keyB) {
    if (keyA === null) return 1;
    if (keyB === null) return -1;
    return keyA - keyB;
  }
  const dueA = a.due_on ? new Date(a.due_on).getTime() : Number.POSITIVE_INFINITY;
  const dueB = b.due_on ? new Date(b.due_on).getTime() : Number.POSITIVE_INFINITY;
  if (dueA !== dueB) {
    return dueA - dueB;
  }
  return (a.number ?? 0) - (b.number ?? 0);
}

/**
 * Annotate every open milestone with `crate`, `stage` (null when untagged),
 * `roadmapBody`, and `releaseOrder`. Closed milestones are dropped.
 */
function classify(milestones) {
  return milestones
    .filter((milestone) => milestone.state !== "closed")
    .map((milestone) => {
      const meta = parseMilestoneRoadmap(milestone.description);
      const releaseOrder = parseReleaseOrder(
        milestone.description,
        `milestone #${milestone.number ?? "?"} (${milestone.title})`,
      );
      return {
        ...milestone,
        crate: crateOf(milestone),
        stage: meta ? meta.stage : null,
        roadmapBody: meta ? meta.body : "",
        releaseOrder,
      };
    });
}

/** Bucket classified milestones into `{now, next, later, other}`, each sorted. */
function bucket(entries) {
  const groups = { now: [], next: [], later: [], other: [] };
  for (const entry of entries) {
    groups[entry.stage ?? "other"].push(entry);
  }
  for (const key of Object.keys(groups)) {
    groups[key].sort(compareMilestones);
  }
  return groups;
}

/** The milestones assigned to `crate`, bucketed by stage (`other` = no `Roadmap:` line). */
function groupByStage(milestones, crate) {
  const wanted = crate.toLowerCase();
  return bucket(classify(milestones).filter((entry) => entry.crate === wanted));
}

/**
 * Group every open milestone by crate, in page order: crates with a
 * `Release-order:` key first (lowest key ascending), then the rest by name,
 * then the `crate: null` group. Returns `[{crate, groups}]`.
 */
function groupByCrate(milestones) {
  const byCrate = new Map();
  for (const entry of classify(milestones)) {
    if (!byCrate.has(entry.crate)) byCrate.set(entry.crate, []);
    byCrate.get(entry.crate).push(entry);
  }
  const minKey = (entries) => {
    const keys = entries.map((e) => e.releaseOrder).filter((k) => k !== null);
    return keys.length > 0 ? Math.min(...keys) : null;
  };
  const crates = [...byCrate.keys()].filter((crate) => crate !== null);
  crates.sort((a, b) => {
    const keyA = minKey(byCrate.get(a));
    const keyB = minKey(byCrate.get(b));
    if (keyA !== keyB) {
      if (keyA === null) return 1;
      if (keyB === null) return -1;
      return keyA - keyB;
    }
    return a < b ? -1 : a > b ? 1 : 0;
  });
  if (byCrate.has(null)) crates.push(null);
  return crates.map((crate) => ({ crate, groups: bucket(byCrate.get(crate)) }));
}

function progressText(milestone) {
  const closed = milestone.closed_issues ?? 0;
  const total = (milestone.open_issues ?? 0) + closed;
  return total === 0 ? "no items yet" : `${closed} of ${total} items done`;
}

/** A `Roadmap:` milestone: heading with its stage, published prose, progress line. */
function renderMilestoneBlock(milestone, crate) {
  const heading = displayTitle(milestone.title, crate);
  const parts = [`#### ${heading} · ${STAGE_HEADING[milestone.stage]}`];
  if (milestone.roadmapBody) {
    parts.push(milestone.roadmapBody);
  }
  parts.push(`${progressText(milestone)} · [follow on GitHub](${milestone.html_url})`);
  return parts.join("\n\n");
}

/** An untagged milestone: one list line, no prose. */
function renderMilestoneLine(milestone, crate) {
  const label = displayTitle(milestone.title, crate).replace(/[[\]]/g, "\\$&");
  return `- [${label}](${milestone.html_url}) · ${progressText(milestone)}`;
}

function renderCrateSection({ crate, groups }) {
  const parts = [`### ${crate ?? WORKSPACE_HEADING}`];
  for (const stage of STAGE_ORDER) {
    for (const milestone of groups[stage]) {
      parts.push(renderMilestoneBlock(milestone, crate));
    }
  }
  if (groups.other.length > 0) {
    const tagged = STAGE_ORDER.some((stage) => groups[stage].length > 0);
    parts.push(tagged ? "Other open milestones:" : "Open milestones:");
    parts.push(groups.other.map((m) => renderMilestoneLine(m, crate)).join("\n"));
  }
  return parts.join("\n\n");
}

/** Render the full generated-region body (no markers) from `milestones`. */
function renderRegion(milestones) {
  const sections = groupByCrate(milestones).map(renderCrateSection);
  if (sections.length === 0) {
    return "_No open milestones._";
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
    if (arg === "--file") args.file = argv[++i];
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
      "Usage: node scripts/roadmap/generate.mjs --file <path> [--fixture <path>] [--check]",
      "",
      "  --file <path>      Repo-relative target markdown file carrying the BEGIN/END markers (required)",
      "  --fixture <path>   Read milestones from this JSON file instead of `gh api` (tests, dry runs)",
      "  --owner/--repo     Override the GitHub owner/repo (default bobmatnyc/trusty-tools)",
      "  --check            Fail with exit 1 on drift instead of writing; writes nothing",
    ].join("\n"),
  );
}

function main(argv) {
  const args = parseArgs(argv);
  if (args.help || !args.file) {
    printUsage();
    process.exitCode = args.help ? 0 : 2;
    return;
  }

  const milestones = loadMilestones(args);
  const region = renderRegion(milestones);
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
  console.log(`Wrote ${args.file} (roadmap region regenerated).`);
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
  AREA_CRATES,
  beginLine,
  endLine,
  parseMilestoneRoadmap,
  parseReleaseOrder,
  crateOf,
  displayTitle,
  compareMilestones,
  classify,
  groupByStage,
  groupByCrate,
  renderMilestoneBlock,
  renderRegion,
  spliceRegion,
  loadMilestones,
  parseArgs,
  main,
};
