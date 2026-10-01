/*
 * Why: the hit viewer highlights source the way the agents GUI file viewer
 * does. `highlight.js/lib/core` plus only the languages the shared map names
 * keeps the console bundle from carrying ~190 grammars it never uses.
 * What: registers each language `LANGUAGES` maps to and exposes
 * `highlightCode(source, path)` over the shared `renderCode`.
 * Test: `HitViewer.test.js`.
 */
import hljs from 'highlight.js/lib/core';
import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cpp from 'highlight.js/lib/languages/cpp';
import csharp from 'highlight.js/lib/languages/csharp';
import css from 'highlight.js/lib/languages/css';
import go from 'highlight.js/lib/languages/go';
import ini from 'highlight.js/lib/languages/ini';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import kotlin from 'highlight.js/lib/languages/kotlin';
import markdown from 'highlight.js/lib/languages/markdown';
import python from 'highlight.js/lib/languages/python';
import ruby from 'highlight.js/lib/languages/ruby';
import rust from 'highlight.js/lib/languages/rust';
import scss from 'highlight.js/lib/languages/scss';
import sql from 'highlight.js/lib/languages/sql';
import swift from 'highlight.js/lib/languages/swift';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';
import { renderCode } from '@foundry/codeView.js';

const GRAMMARS = {
  bash, c, cpp, csharp, css, go, ini, java, javascript, json, kotlin,
  markdown, python, ruby, rust, scss, sql, swift, typescript, xml, yaml,
};
for (const [name, grammar] of Object.entries(GRAMMARS)) hljs.registerLanguage(name, grammar);

/**
 * Highlighted HTML for `source`, chosen by `path`'s extension; escaped plain
 * text when the extension is unknown.
 * @param {string} source
 * @param {string} path
 */
export function highlightCode(source, path) {
  return renderCode(hljs, source, path);
}
