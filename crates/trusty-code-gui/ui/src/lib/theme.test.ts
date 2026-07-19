// Why: issue #3133 — light + dark themes following `prefers-color-scheme`,
// with no manual toggle. The actual theming lives in two places:
// `tailwind.config.js` (maps `trusty-*`/`status-*` utility classes onto CSS
// custom properties) and `src/app.css` (defines the light-default `:root`
// values and the `@media (prefers-color-scheme: dark)` override). A
// regression here — a token reverting to a hardcoded hex literal, or the
// dark block silently matching the light one — would be invisible in a
// normal component test (there's no DOM assertion that exercises a media
// query), so this file asserts the actual config/CSS content directly.
// What: `tailwindConfig` colors must all be `rgb(var(--color-*) / <alpha-value>)`
// references — NOT a bare `var(--color-*)`, which silently compiles zero
// opacity-modifier rules (a regression this file specifically pins, since
// every component relies on `bg-status-ok/15`-style modifiers). `app.css`
// must define both a `:root` block and a `prefers-color-scheme: dark`
// override for every variable Tailwind references, with the dark values
// genuinely differing from light for the primary surface/text/border
// tokens. Also scans every component `.svelte` file for a `<style>` block
// or an inline `style="..."` carrying a hardcoded color literal, and for
// any raw (non-themed) Tailwind palette utility — this codebase has none
// today (all theming flows through the `trusty-*`/`status-*` tokens), and
// this guards that staying true.
// Test: this file. Manual verification: toggle System Settings > Appearance
// while `pnpm tauri dev` is running and confirm the window recolors live.
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import postcss from 'postcss';
import tailwindcss from 'tailwindcss';
import { describe, expect, it } from 'vitest';
import tailwindConfig from '../../tailwind.config.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const uiRoot = resolve(__dirname, '../..');
const appCss = readFileSync(resolve(uiRoot, 'src/app.css'), 'utf-8');

function colorTokens(): Record<string, string> {
  return tailwindConfig.theme.extend.colors as Record<string, string>;
}

function block(css: string, pattern: RegExp): string {
  const match = css.match(pattern);
  if (!match) throw new Error(`pattern not found: ${pattern}`);
  return match[1];
}

describe('theme tokens (tailwind.config.js) — issue #3133', () => {
  it('every trusty-*/status-* color uses the rgb(var(--color-*) / <alpha-value>) format, never a hardcoded literal or a bare var()', () => {
    // The bare `var(--color-*)` form (no `rgb(... / <alpha-value>)`
    // wrapper) is a real regression this test would have caught: it
    // compiles fine for the base utility (`bg-status-ok`) but silently
    // produces NO rule at all for any opacity-modified one
    // (`bg-status-ok/15`) — Tailwind can't manipulate an opaque `var()`
    // value's alpha channel at build time.
    const colors = colorTokens();
    expect(Object.keys(colors).length).toBeGreaterThan(0);
    for (const [name, value] of Object.entries(colors)) {
      expect(
        value,
        `color token "${name}" should be rgb(var(--color-*) / <alpha-value>)`,
      ).toMatch(/^rgb\(var\(--color-[a-z-]+\) \/ <alpha-value>\)$/);
    }
  });

  it('does not force a class-based dark mode strategy (no manual toggle exists to key off)', () => {
    expect(tailwindConfig.darkMode).toBeUndefined();
  });

  it('compiles opacity-modified utilities to a real rule, not a silently-missing one — the actual regression a bare var() causes', async () => {
    // Runs the REAL tailwind.config.js through postcss/tailwindcss against
    // a tiny virtual template, exactly like the production build does.
    // This is the one test that would have caught the original bug: with
    // `colors: { 'status-ok': 'var(--color-status-ok)' }`, this compiles to
    // zero bytes of output for `bg-status-ok/15` — no error, no warning,
    // just a missing rule — because Tailwind cannot inject an alpha value
    // into an opaque `var()`. `pnpm build`'s raw CSS output showed exactly
    // that silent gap during this issue's investigation.
    const result = await postcss([
      tailwindcss({
        ...tailwindConfig,
        content: [
          { raw: '<div class="bg-status-ok/15 text-trusty-text/60"></div>', extension: 'html' },
        ],
      }),
    ]).process('@tailwind utilities;', { from: undefined });

    // Un-minified postcss output formatting (whitespace, leading zero on
    // the alpha value) is incidental — assert the substance: each selector
    // exists and its declaration references the variable with a non-zero
    // alpha fraction, not an empty/missing rule.
    expect(result.css).toMatch(/\.bg-status-ok\\\/15\s*{[^}]*rgb\(var\(--color-status-ok\) \/ 0?\.15\)/);
    expect(result.css).toMatch(/\.text-trusty-text\\\/60\s*{[^}]*rgb\(var\(--color-text\) \/ 0?\.6\)/);
  });
});

describe('theme values (src/app.css) — issue #3133', () => {
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

  it('defines a prefers-color-scheme: dark override for every color that actually differs by theme', () => {
    // `--color-primary` is intentionally the same brand color in both
    // themes (never overridden — it simply cascades from `:root`), so this
    // checks the surface/border/text/status set that genuinely needs a
    // dark-specific value, not every Tailwind-referenced variable.
    const darkBlock = block(
      appCss,
      /@media\s*\(prefers-color-scheme:\s*dark\)\s*{\s*:root\s*{([^}]*)}/,
    );
    const mustDiffer = [
      'color-surface',
      'color-border',
      'color-text',
      'color-status-ok',
      'color-status-error',
      'color-status-warn',
    ];
    for (const varName of mustDiffer) {
      expect(darkBlock, `dark override should define --${varName}`).toMatch(
        new RegExp(`--${varName}\\s*:`),
      );
    }
  });

  it('the dark override genuinely differs from the light default for surface/text/border (real theming, not a no-op)', () => {
    const rootBlock = block(appCss, /:root\s*{([^}]*)}/);
    const darkBlock = block(
      appCss,
      /@media\s*\(prefers-color-scheme:\s*dark\)\s*{\s*:root\s*{([^}]*)}/,
    );
    const valueOf = (css: string, varName: string): string | undefined =>
      css.match(new RegExp(`--${varName}\\s*:\\s*([^;]+);`))?.[1]?.trim();

    for (const varName of ['color-surface', 'color-text', 'color-border']) {
      const light = valueOf(rootBlock, varName);
      const dark = valueOf(darkBlock, varName);
      expect(light, `--${varName} missing from :root`).toBeTruthy();
      expect(dark, `--${varName} missing from the dark override`).toBeTruthy();
      expect(dark, `--${varName} should differ between light and dark`).not.toBe(light);
    }
  });
});

describe('component theming hygiene — issue #3133', () => {
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
    // Caught a real instance during the #3133 audit: HealthPanel.svelte's
    // code-block background was `bg-black/30` — Tailwind's built-in
    // `black`, which never changes with `prefers-color-scheme`, unlike the
    // `trusty-*`/`status-*` tokens this file's other tests pin to
    // `var(--color-*)`. Fixed to `bg-trusty-border/20`; this test guards
    // against the same class of regression reappearing anywhere else.
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
});
