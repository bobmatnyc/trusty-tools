/*
 * Why: the hit viewer highlights source the way the agents GUI file viewer
 * does, but most dashboard visits never open it. Loading highlight.js and its
 * grammars on demand keeps them out of the main bundle; each grammar loads
 * the first time a hit in that language is opened.
 * What: `highlightCode(source, path)` resolves to the shared `renderCode`
 * output. It imports `highlight.js/lib/core` and the one grammar the path's
 * extension maps to (`LANGUAGES`), registering it once; an unknown extension
 * or a failed chunk load resolves to escaped plain text.
 * Test: `HitViewer.test.js`.
 */
import { LANGUAGES, escapeHtml, renderCode } from '@foundry/codeView.js';

// One literal import() per grammar so Vite emits one chunk per language.
const GRAMMARS = {
  bash: () => import('highlight.js/lib/languages/bash'),
  c: () => import('highlight.js/lib/languages/c'),
  cpp: () => import('highlight.js/lib/languages/cpp'),
  csharp: () => import('highlight.js/lib/languages/csharp'),
  css: () => import('highlight.js/lib/languages/css'),
  go: () => import('highlight.js/lib/languages/go'),
  ini: () => import('highlight.js/lib/languages/ini'),
  java: () => import('highlight.js/lib/languages/java'),
  javascript: () => import('highlight.js/lib/languages/javascript'),
  json: () => import('highlight.js/lib/languages/json'),
  kotlin: () => import('highlight.js/lib/languages/kotlin'),
  markdown: () => import('highlight.js/lib/languages/markdown'),
  python: () => import('highlight.js/lib/languages/python'),
  ruby: () => import('highlight.js/lib/languages/ruby'),
  rust: () => import('highlight.js/lib/languages/rust'),
  scss: () => import('highlight.js/lib/languages/scss'),
  sql: () => import('highlight.js/lib/languages/sql'),
  swift: () => import('highlight.js/lib/languages/swift'),
  typescript: () => import('highlight.js/lib/languages/typescript'),
  xml: () => import('highlight.js/lib/languages/xml'),
  yaml: () => import('highlight.js/lib/languages/yaml')
};

let core = null;

/**
 * Highlighted HTML for `source`, chosen by `path`'s extension.
 * @param {string} source
 * @param {string} path
 * @returns {Promise<string>}
 */
export async function highlightCode(source, path) {
  const language = LANGUAGES[path.split('.').pop()?.toLowerCase() ?? ''];
  const load = language ? GRAMMARS[language] : undefined;
  if (!load) return escapeHtml(source);
  try {
    core ??= (await import('highlight.js/lib/core')).default;
    if (!core.getLanguage(language)) core.registerLanguage(language, (await load()).default);
    return renderCode(core, source, path);
  } catch (e) {
    console.warn(`[hit viewer] highlighting ${language} failed; showing plain text`, e);
    return escapeHtml(source);
  }
}
