// Shared mock data for the Trusty Search screens.
export const searchNav = (active) => ({
  title: 'TRUSTY SEARCH',
  unit: 'UNIT-01 · CODE SEARCH',
  accent: '#b7410e',
  mark: { body: '#b7410e', eyes: 'square' },
  items: [
    { icon: '◇', label: 'Dashboard' },
    { icon: '⌕', label: 'Search' },
    { icon: '▣', label: 'Indexes' },
    { icon: '♥', label: 'Health' },
    { icon: '☰', label: 'Logs' },
    { icon: '⚙', label: 'Config' }
  ].map((it) => ({ ...it, active: it.label === active }))
});

export const indexes = [
  { name: 'trusty-tools', docs: '48,213', disk: '412.6 MB', last: 'Jul 18, 09:14', path: '/Users/mo/code/trusty-tools', status: 'ready', selected: true },
  { name: 'memory-palace', docs: '31,870', disk: '268.1 MB', last: 'Jul 17, 18:40', path: '/Users/mo/code/memory-palace', status: 'working', progress: '1,204 / 31,870', selected: true },
  { name: 'gitflow-rs', docs: '27,904', disk: '231.9 MB', last: 'Jul 12, 14:03', path: '/Users/mo/code/gitflow-rs', status: 'error', selected: false },
  { name: 'docs-site', docs: '20,454', disk: '166.3 MB', last: 'Jul 16, 11:52', path: '/Users/mo/code/docs-site', status: 'ready', selected: false }
];

export const memoryNav = (active) => ({
  title: 'TRUSTY MEMORY',
  unit: 'UNIT-02 · MEMORY PALACE',
  accent: '#8a5a2b',
  mark: { body: '#8a5a2b', eyes: 'round' },
  items: [
    { icon: '▦', label: 'Palaces' },
    { icon: '◈', label: 'Graph' },
    { icon: '☾', label: 'Dream' },
    { icon: '♥', label: 'Health' },
    { icon: '☰', label: 'Logs' }
  ].map((it) => ({ ...it, active: it.label === active }))
});
