/** @type {import('tailwindcss').Config} */
export default {
  darkMode: 'class',
  content: ['./index.html', './src/**/*.{svelte,js,ts}'],
  theme: {
    extend: {
      colors: {
        'trusty-primary': '#4f46e5', // indigo-600
        'trusty-surface': '#0f172a', // slate-900 (dark)
        'trusty-surface-light': '#ffffff', // white (light)
        'trusty-border': '#334155', // slate-700 (dark)
        'trusty-border-light': '#e2e8f0', // slate-200 (light)
        'trusty-text': '#f1f5f9', // slate-100 (dark)
        'trusty-text-light': '#0f172a', // slate-900 (light)
        'status-ok': '#22c55e', // green-500
        'status-error': '#ef4444', // red-500
        'status-warn': '#f59e0b', // amber-500 — DOC-39 §8: amber = inferred/warming
        'status-neutral': '#64748b', // slate-500 — no-session / never-probed
      },
    },
  },
  plugins: [],
};
