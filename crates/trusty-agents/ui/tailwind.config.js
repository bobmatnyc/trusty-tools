/** @type {import('tailwindcss').Config} */
export default {
  darkMode: 'class',
  content: [
    './index.html',
    './src/**/*.{svelte,js,ts}',
  ],
  theme: {
    extend: {
      colors: {
        ompm: {
          // Dark mode values (defaults — open-mpm was dark-first).
          bg: '#0F1221',
          surface: '#1A1F3A',
          text: '#E6E9F5',
          muted: '#8892B0',
          border: '#2A2F4A',
          primary: '#3B4CCA',
          teal: '#2EC4B6',
          amber: '#FF9F1C',
          purple: '#6C5CE7',
          // Light mode variants (used via `dark:` prefix inversion).
          'light-bg': '#F4F6FB',
          'light-surface': '#FFFFFF',
          'light-text': '#1A1F3A',
          'light-muted': '#5C6480',
          'light-border': '#D1D5E8',
        },
      },
      fontFamily: {
        // Inter and JetBrains Mono are declared via @font-face in index.html
        // pointing at local system aliases. The fallback chain ensures text
        // renders immediately with system fonts when no exact match exists.
        sans: ['Inter', '-apple-system', 'BlinkMacSystemFont', '"Segoe UI"', 'system-ui', 'sans-serif'],
        mono: ['"JetBrains Mono"', '"SF Mono"', 'Monaco', '"Cascadia Code"', 'Consolas', 'ui-monospace', 'monospace'],
      },
    },
  },
  plugins: [],
};
