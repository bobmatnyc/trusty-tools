import { describe, expect, it } from 'vitest';
import { renderMarkdown, renderCode, relativeDocumentPath } from './fileRendering';

describe('file rendering', () => {
  it('renders Markdown without executable content or remote resources', () => {
    const html = renderMarkdown('# Title\n\n**Bold** and `code`\n\n<script>alert(1)</script><img src="https://example.com/tracker" onerror="alert(1)"><iframe src="https://example.com"></iframe>\n\n[bad](javascript:alert(1))');
    const host = document.createElement('div'); host.innerHTML = html;
    expect(host.querySelector('h1')?.textContent).toBe('Title');
    expect(host.querySelector('strong')?.textContent).toBe('Bold');
    expect(host.querySelector('script,img,iframe')).toBeNull();
    expect(html).not.toContain('javascript:');
    expect(html).not.toContain('onerror');
  });
  it('escapes executable markup in highlighted code and unknown file types', () => {
    for (const path of ['example.js', 'unknown.data']) {
      const host = document.createElement('div'); host.innerHTML = renderCode('<img src=x onerror=alert(1)>', path);
      expect(host.querySelector('img')).toBeNull();
      expect(host.textContent).toBe('<img src=x onerror=alert(1)>');
    }
  });
  it('resolves document links only within the selected root', () => {
    expect(relativeDocumentPath('docs/README.md', '../src/main.rs')).toBe('src/main.rs');
    for (const href of ['../../secret', '/etc/passwd', 'https://example.com', 'javascript:alert(1)', '%2Fetc%2Fpasswd', '..\\secret', '%00', '%broken']) {
      expect(relativeDocumentPath('docs/README.md', href)).toBeNull();
    }
  });
});
