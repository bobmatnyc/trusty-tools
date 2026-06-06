import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './tests',
  timeout: 30_000,
  use: {
    // Use 127.0.0.1 not localhost — on macOS localhost resolves IPv6 first
    // ([::1]) which is refused, causing a ~15s fallback delay per test.
    baseURL: process.env.OMPM_URL ?? 'http://127.0.0.1:7654',
    headless: true,
  },
  reporter: [['list'], ['html', { open: 'never' }]],
});
