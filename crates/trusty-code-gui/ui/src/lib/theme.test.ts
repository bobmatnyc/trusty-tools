// Why: Foundry retheme (issue #3153, docs/design/UI/design-system/). The
// actual theming lives in three places: `tailwind.config.js` (maps
// `trusty-*`/`status-*` utility classes onto CSS custom properties),
// `src/app.css` (defines the light-default `:root` values and the
// `[data-theme='dark']` override), and `lib/theme-bootstrap.ts` (the OS
// appearance → `data-theme` attribute bridge, DOC-39 addendum AC-27). A
// regression here — a token reverting to a hardcoded hex literal, the dark
// block silently matching the light one, or activation reverting to a bare
// `prefers-color-scheme` media query — would be invisible in a normal
// component test (there's no DOM assertion that exercises a media query or
// attribute), so this file asserts the actual config/CSS/bootstrap content
// and behavior directly.
// What: `tailwindConfig` colors must all be `rgb(var(--color-*) / <alpha-value>)`
// references. `app.css` must define both a `:root` block and a
// `[data-theme='dark']` override for every variable Tailwind references,
// with the dark values genuinely differing from light for every token that
// is supposed to (Foundry's accent itself brightens one step in dark, unlike
// the prior #3133 palette which kept it constant — see `mustDiffer` below).
// `theme-bootstrap.ts` is exercised directly against a mocked `matchMedia`:
// it must set `data-theme="dark"` immediately when the OS is dark, remove it
// when light, and keep tracking a live `change` event. Also scans every
// component `.svelte` file for a `<style>` block, an inline hardcoded color,
// or a raw (non-themed) Tailwind palette utility.
// Test: this file. Manual verification: toggle System Settings > Appearance
// while `pnpm tauri dev` is running and confirm the window recolors live.
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import postcss from 'postcss';
import tailwindcss from 'tailwindcss';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import tailwindConfig from '../../tailwind.config.js';
import { applyTheme, initThemeBootstrap } from './theme-bootstrap';

const __dirname = dirname(fileURLToPath(import.meta.url));
const uiRoot = resolve(__dirname, '../..');
const appCss = readFileSync(resolve(uiRoot, 'src/app.css'), 'utf-8');
const indexHtml = readFileSync(resolve(uiRoot, 'index.html'), 'utf-8');

function colorTokens(): Record<string, string> {
  return tailwindConfig.theme.extend.colors as Record<string, string>;
}

function block(css: string, pattern: RegExp): string {
  const match = css.match(pattern);
  if (!match) throw new Error(`pattern not found: ${pattern}`);
  return match[1];
}

describe('theme tokens (tailwind.config.js) — issue #3153', () => {
  it('every trusty-*/status-* color uses the rgb(var(--color-*) / <alpha-value>) format, never a hardcoded literal or a bare var()', () => {
    const colors = colorTokens();
    expect(Object.keys(colors).length).toBeGreaterThan(0);
    for (const [name, value] of Object.entries(colors)) {
      expect(
        value,
        `color token "${name}" should be rgb(var(--color-*) / <alpha-value>)`,
      ).toMatch(/^rgb\(var\(--color-[a-z-]+\) \/ <alpha-value>\)$/);
    }
  });

  it('does not force a class-based dark mode strategy (activation is the data-theme attribute, not a dark: variant)', () => {
    expect(tailwindConfig.darkMode).toBeUndefined();
  });

  it('compiles opacity-modified utilities to a real rule, not a silently-missing one — the actual regression a bare var() causes', async () => {
    const result = await postcss([
      tailwindcss({
        ...tailwindConfig,
        content: [
          { raw: '<div class="bg-status-ok/15 text-trusty-text/60"></div>', extension: 'html' },
        ],
      }),
    ]).process('@tailwind utilities;', { from: undefined });

    expect(result.css).toMatch(/\.bg-status-ok\\\/15\s*{[^}]*rgb\(var\(--color-status-ok\) \/ 0?\.15\)/);
    expect(result.css).toMatch(/\.text-trusty-text\\\/60\s*{[^}]*rgb\(var\(--color-text\) \/ 0?\.6\)/);
  });
});

describe('theme values (src/app.css) — issue #3153 Foundry palette', () => {
  it('sets color-scheme: light dark so native controls also follow the system', () => {
    expect(appCss).toMatch(/color-scheme:\s*light dark/);
  });

  it('defines a :root light-default value for every CSS variable Tailwind references', () => {
    const rootBlock = block(appCss, /:root\s*{([^}]*)}/);
    for (const value of Object.values(colorTokens())) {
      const varName = value.match(/--([a-z-]+)/)![1];
      expect(rootBlock, `:root should define --${varName}`).toMatch(
        new RegExp(`--${varName}\\s*:`),
      );
    }
  });

  it("defines a [data-theme='dark'] override — not a bare prefers-color-scheme media query — for every color", () => {
    // AC-27: activation is attribute-based. A regression back to a bare
    // `@media (prefers-color-scheme: dark)` block (the #3133 mechanism this
    // replaces) would fail this, since it greps for the attribute selector
    // specifically.
    expect(appCss).toMatch(/\[data-theme=['"]dark['"]\]\s*{/);
    const darkBlock = block(appCss, /\[data-theme=['"]dark['"]\]\s*{([^}]*)}/);
    for (const value of Object.values(colorTokens())) {
      const varName = value.match(/--([a-z-]+)/)![1];
      expect(darkBlock, `[data-theme='dark'] should define --${varName}`).toMatch(
        new RegExp(`--${varName}\\s*:`),
      );
    }
  });

  it('the dark override genuinely differs from the light default for every themed token (real theming, not a no-op)', () => {
    // Unlike the prior #3133 palette (which kept --color-primary constant
    // across themes), Foundry's rust accent brightens one step in dark
    // (#B7410E -> #D97742, tokens.css), so every token this file defines is
    // expected to differ...
    //
    // ...EXCEPT the two `.wsrail` chassis ink tokens (issue #3153 shell
    // rebuild): `sidebar-text`/`sidebar-muted` are a dark chassis's own ink
    // colors, not a themed surface — the rail stays a dark chassis in BOTH
    // app themes (docs/design/UI/design-system/tokens.css ships the same
    // hex for both), so there is nothing to invert. `sidebar-bg`/
    // `sidebar-border`/`sidebar-active` still differ (the chassis itself
    // steps one shade darker in Night Shift) and are NOT exempted.
    const CONSTANT_ACROSS_THEMES = new Set(['color-sidebar-text', 'color-sidebar-muted']);
    const rootBlock = block(appCss, /:root\s*{([^}]*)}/);
    const darkBlock = block(appCss, /\[data-theme=['"]dark['"]\]\s*{([^}]*)}/);
    const valueOf = (css: string, varName: string): string | undefined =>
      css.match(new RegExp(`--${varName}\\s*:\\s*([^;]+);`))?.[1]?.trim();

    for (const value of Object.values(colorTokens())) {
      const varName = value.match(/--([a-z-]+)/)![1];
      const light = valueOf(rootBlock, varName);
      const dark = valueOf(darkBlock, varName);
      expect(light, `--${varName} missing from :root`).toBeTruthy();
      expect(dark, `--${varName} missing from [data-theme='dark']`).toBeTruthy();
      if (CONSTANT_ACROSS_THEMES.has(varName)) {
        expect(dark, `--${varName} is a chassis ink token, expected constant`).toBe(light);
      } else {
        expect(dark, `--${varName} should differ between light and dark`).not.toBe(light);
      }
    }
  });
});

describe('font bundling (index.html) — Foundry typography', () => {
  it('self-hosts the three Foundry faces from public/fonts/, never a remote Google Fonts link', () => {
    // Checks for an actual remote-loading construct (a <link>/@import
    // pulling from the CDN), not just any textual mention — this file's own
    // comments legitimately document what the self-hosted files were copied
    // from and why a CDN link is deliberately absent.
    expect(indexHtml).not.toMatch(/<link[^>]+fonts\.(googleapis|gstatic)\.com/);
    expect(indexHtml).not.toMatch(/@import\s+url\(['"]?https?:\/\/fonts\.(googleapis|gstatic)\.com/);
    expect(indexHtml).toMatch(/@font-face/);
    for (const family of ['Chakra Petch', 'IBM Plex Sans', 'IBM Plex Mono']) {
      expect(indexHtml, `index.html should declare @font-face for ${family}`).toMatch(
        new RegExp(`font-family:\\s*'${family}'`),
      );
    }
  });
});

describe('theme activation (lib/theme-bootstrap.ts) — DOC-39 addendum AC-27', () => {
  function mockMatchMedia(initialMatches: boolean) {
    let listener: ((event: { matches: boolean }) => void) | null = null;
    const query = {
      matches: initialMatches,
      addEventListener: vi.fn((_event: string, cb: (event: { matches: boolean }) => void) => {
        listener = cb;
      }),
    };
    vi.stubGlobal(
      'matchMedia',
      vi.fn().mockReturnValue(query),
    );
    return {
      query,
      fireChange: (matches: boolean) => listener?.({ matches }),
    };
  }

  beforeEach(() => {
    document.documentElement.removeAttribute('data-theme');
  });

  afterEach(() => {
    document.documentElement.removeAttribute('data-theme');
    vi.unstubAllGlobals();
  });

  it('applyTheme sets data-theme="dark" when true and removes it when false', () => {
    applyTheme(true);
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
    applyTheme(false);
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });

  it('initThemeBootstrap applies the current OS match immediately', () => {
    mockMatchMedia(true);
    initThemeBootstrap();
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('initThemeBootstrap does not set data-theme when the OS is light', () => {
    mockMatchMedia(false);
    initThemeBootstrap();
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });

  it('a live OS-appearance change updates data-theme without a reload', () => {
    const { fireChange } = mockMatchMedia(false);
    initThemeBootstrap();
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);

    fireChange(true);
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');

    fireChange(false);
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });

  it('is a no-op (does not throw) when matchMedia is unavailable', () => {
    vi.stubGlobal('matchMedia', undefined);
    expect(() => initThemeBootstrap()).not.toThrow();
    expect(document.documentElement.hasAttribute('data-theme')).toBe(false);
  });
});

describe('component theming hygiene — issue #3153', () => {
  const svelteFiles: { name: string; content: string }[] = [
    { name: 'App.svelte', content: readFileSync(resolve(uiRoot, 'src/App.svelte'), 'utf-8') },
    ...readdirSync(resolve(uiRoot, 'src/components'))
      .filter((f) => f.endsWith('.svelte'))
      .map((f) => ({
        name: f,
        content: readFileSync(resolve(uiRoot, 'src/components', f), 'utf-8'),
      })),
  ];

  it('no component defines its own <style> block (all theming flows through Tailwind tokens)', () => {
    expect(svelteFiles.length).toBeGreaterThan(0);
    for (const { name, content } of svelteFiles) {
      expect(content, `${name} should not define a <style> block`).not.toMatch(/<style[\s>]/);
    }
  });

  it('no component uses an inline style="" attribute carrying a hardcoded color literal', () => {
    for (const { name, content } of svelteFiles) {
      const styleAttrs = content.match(/style="[^"]*"/g) ?? [];
      for (const attr of styleAttrs) {
        expect(attr, `${name} has a hardcoded color in an inline style`).not.toMatch(
          /#[0-9a-fA-F]{3,8}\b|rgba?\(/,
        );
      }
    }
  });

  it('no component uses a raw (non-themed) Tailwind palette color utility — every color routes through a trusty-*/status-* token', () => {
    const HARDCODED_PALETTE =
      /\b(?:bg|text|border|ring|from|via|to|fill|stroke)-(?:black|white|slate|gray|zinc|neutral|stone|red|orange|amber|yellow|lime|green|emerald|teal|cyan|sky|blue|indigo|violet|purple|fuchsia|pink|rose)(?:-[0-9]+)?\b/;
    for (const { name, content } of svelteFiles) {
      const match = content.match(HARDCODED_PALETTE);
      expect(
        match,
        `${name} uses a raw Tailwind palette color (${match?.[0]}) instead of a trusty-*/status-* theme token`,
      ).toBeNull();
    }
  });

  it('no component hardcodes a hex color anywhere in its markup or script (tokens only, per the Foundry README guardrail)', () => {
    for (const { name, content } of svelteFiles) {
      const match = content.match(/#[0-9a-fA-F]{3}\b|#[0-9a-fA-F]{6}\b|#[0-9a-fA-F]{8}\b/);
      expect(
        match,
        `${name} hardcodes a hex color (${match?.[0]}) — a component that hardcodes a hex breaks theming`,
      ).toBeNull();
    }
  });
});
