// Canonical source: docs/design/UI/design-system/components/codeView.js.
// Vendored byte-for-byte into crates/trusty-agents/ui/src/lib/foundry/ — edit
// here, then copy; that crate's `vendored.test.ts` fails on any drift.

/*
 * Why: the agents GUI file viewer and the search dashboard's hit viewer both
 * render source text with syntax highlighting and unified diffs. One copy of
 * the language map and the line classifier keeps the two views identical.
 * What: pure helpers with no package imports — the caller passes its own
 * highlight.js instance, so this file resolves from any package without a
 * bare-module import reaching outside that package's node_modules.
 * Test: `codeView.test.js` in crates/trusty-console/ui-search/src/lib/ and
 * `fileRendering.test.ts` in crates/trusty-agents/ui/src/lib/.
 */

/** @type {Record<string, string>} */
const ESCAPES = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };

/**
 * Escapes the five HTML-significant characters.
 * @param {string} text
 * @returns {string}
 */
export function escapeHtml(text) {
  return text.replace(/[&<>"']/g, (c) => ESCAPES[c]);
}

/**
 * File extension → highlight.js language name. A package that registers
 * languages on `highlight.js/lib/core` registers exactly these names.
 * @type {Record<string, string>}
 */
export const LANGUAGES = {
  rs: 'rust', ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript',
  py: 'python', rb: 'ruby', go: 'go', java: 'java', c: 'c', h: 'c', cpp: 'cpp',
  cs: 'csharp', html: 'xml', xml: 'xml', css: 'css', scss: 'scss', json: 'json',
  yml: 'yaml', yaml: 'yaml', toml: 'ini', sh: 'bash', zsh: 'bash', sql: 'sql',
  swift: 'swift', kt: 'kotlin', md: 'markdown', markdown: 'markdown',
};

/** Above this size, text renders escaped but unhighlighted so the UI stays responsive. */
export const MAX_HIGHLIGHT_CHARS = 200_000;

/**
 * @typedef {{ highlight(code: string, options: { language: string, ignoreIllegals?: boolean }): { value: string } }} Highlighter
 */

/**
 * Why: highlighting is the expensive, failure-prone step; it must never throw
 * into a view or block it on very large text.
 * What: returns HTML for `source`, highlighted by the language its `path`
 * extension maps to, or plain escaped text for an unknown extension, an
 * oversized source, or a highlighter error.
 * Test: `renderCode escapes unknown and oversized sources` in codeView.test.js.
 * @param {Highlighter} hljs
 * @param {string} source
 * @param {string} path
 * @returns {string}
 */
export function renderCode(hljs, source, path) {
  const language = LANGUAGES[path.split('.').pop()?.toLowerCase() ?? ''];
  if (!language || source.length > MAX_HIGHLIGHT_CHARS) return escapeHtml(source);
  try {
    return hljs.highlight(source, { language, ignoreIllegals: true }).value;
  } catch {
    return escapeHtml(source);
  }
}

/**
 * Classifies one unified-diff line for styling. File headers (`+++`, `---`)
 * are context, not changes.
 * @param {string} line
 * @returns {'added' | 'removed' | 'hunk' | 'context'}
 */
export function diffLineKind(line) {
  if (line.startsWith('+') && !line.startsWith('+++')) return 'added';
  if (line.startsWith('-') && !line.startsWith('---')) return 'removed';
  if (line.startsWith('@@')) return 'hunk';
  return 'context';
}

/**
 * Line numbers for a gutter: `count` numbers from `start`, newline-joined.
 * A trailing newline in the source does not add a numbered line.
 * @param {number} start
 * @param {string} html
 * @returns {string}
 */
export function gutterNumbers(start, html) {
  const lines = html.split('\n');
  const count = lines.length > 1 && lines[lines.length - 1] === '' ? lines.length - 1 : lines.length;
  return Array.from({ length: count }, (_, i) => String(start + i)).join('\n');
}
