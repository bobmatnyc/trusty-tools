import { Marked, Renderer } from 'marked';
import DOMPurify from 'dompurify';
import hljs from 'highlight.js';

const escape = (text: string) => text.replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]!));
const renderer = new Renderer();
// Documents must not fetch external resources or navigate the desktop webview.
// Relative links are handled explicitly by the viewer within its selected root.
renderer.image = ({ text }) => `<span class="document-image-label">[Image: ${escape(text)}]</span>`;
renderer.link = function ({ href, tokens }) {
  const text = this.parser.parseInline(tokens);
  return /^(?:[a-z][a-z\d+.-]*:|\/|\\)/i.test(href)
    ? `<span>${text}</span>`
    : `<a role="link" tabindex="0" data-local-href="${escape(href)}">${text}</a>`;
};
const markdown = new Marked({ renderer, gfm: true, breaks: false });
export function renderMarkdown(source: string): string {
  return DOMPurify.sanitize(markdown.parse(source, { async: false }) as string, {
    USE_PROFILES: { html: true },
    FORBID_TAGS: ['img', 'style', 'form', 'input', 'button', 'video', 'audio', 'iframe', 'object'],
    FORBID_ATTR: ['style', 'src', 'srcset', 'id', 'name', 'href'],
  });
}
const languages: Record<string, string> = {
  rs: 'rust', ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript',
  py: 'python', rb: 'ruby', go: 'go', java: 'java', c: 'c', h: 'c', cpp: 'cpp',
  cs: 'csharp', html: 'xml', xml: 'xml', css: 'css', scss: 'scss', json: 'json',
  yml: 'yaml', yaml: 'yaml', toml: 'ini', sh: 'bash', zsh: 'bash', sql: 'sql',
  swift: 'swift', kt: 'kotlin', md: 'markdown', markdown: 'markdown',
};
export function renderCode(source: string, path: string): string {
  const language = languages[path.split('.').pop()?.toLowerCase() ?? ''];
  // Large text remains readable without blocking the UI on syntax analysis.
  if (!language || source.length > 200_000) return escape(source);
  try { return hljs.highlight(source, { language, ignoreIllegals: true }).value; }
  catch { return escape(source); }
}
export function relativeDocumentPath(current: string, href: string): string | null {
  let decoded: string;
  try { decoded = decodeURIComponent(href.split('#')[0]); } catch { return null; }
  if (!decoded || /^(?:[a-z][a-z\d+.-]*:|\/|\\)/i.test(decoded) || decoded.includes('\\') || decoded.includes('\0')) return null;
  const parts = current.split('/').slice(0, -1);
  for (const part of decoded.split('/')) {
    if (!part || part === '.') continue;
    if (part === '..') { if (!parts.length) return null; parts.pop(); }
    else parts.push(part);
  }
  return parts.join('/');
}
