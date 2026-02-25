import type { IconName } from '../components/Icon';

export interface NavItem {
  id: string;
  label: string;
  icon: IconName;
  path: string;
  count?: string;
}

export interface NavGroup {
  group: string;
  items: NavItem[];
}

// mirrors design's NAV. `path` added so router + sidebar share one source
export const NAV: NavGroup[] = [
  {
    group: 'OVERVIEW',
    items: [
      { id: 'scan', label: 'Smart Scan', icon: 'bolt', path: '/scan' },
      { id: 'disk', label: 'Disk Usage', icon: 'pie', path: '/disk' },
    ],
  },
  {
    group: 'CLEANUP',
    items: [
      { id: 'junk', label: 'System Junk', icon: 'broom', path: '/junk', count: '4.2 GB' },
      { id: 'dupes', label: 'Duplicates', icon: 'copy', path: '/dupes', count: '1,284' },
      { id: 'large', label: 'Large & Old', icon: 'archive', path: '/large', count: '87' },
      { id: 'privacy', label: 'Privacy', icon: 'shield', path: '/privacy' },
    ],
  },
  {
    group: 'MAINTENANCE',
    items: [
      { id: 'startup', label: 'Startup Items', icon: 'power', path: '/startup' },
      { id: 'memory', label: 'Memory', icon: 'chip', path: '/memory' },
      { id: 'activity', label: 'Activity', icon: 'pulse', path: '/activity' },
    ],
  },
  {
    group: 'SECURITY',
    items: [
      { id: 'malware', label: 'Malware Scan', icon: 'shield2', path: '/malware' },
    ],
  },
  {
    group: 'SAFAI',
    items: [
      { id: 'settings', label: 'Settings', icon: 'settings', path: '/settings' },
    ],
  },
];

export const FALLBACK_PATH = '/scan';

export function findNavItem(path: string): { item: NavItem; group: string } | undefined {
  for (const g of NAV) {
    const item = g.items.find((i) => i.path === path);
    if (item) return { item, group: g.group };
  }
  return undefined;
}
