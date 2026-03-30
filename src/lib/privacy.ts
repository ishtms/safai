// ts mirror of src-tauri/src/scanner/privacy. camelCase structs, kebab
// serde enums for browser + category ids. rust tests
// id_strings_round_trip_to_serde + serialization_is_camel_case guard
// the contract, rename fields on both sides or break the wire

import { invoke } from './ipc';

export type BrowserId =
  | 'chrome'
  | 'chromium'
  | 'edge'
  | 'brave'
  | 'vivaldi'
  | 'firefox'
  | 'safari';

export type PrivacyCategoryId =
  | 'cache'
  | 'cookies'
  | 'history'
  | 'sessions'
  | 'local-storage';

export interface PrivacyTarget {
  path: string;
  bytes: number;
  fileCount: number;
  /** unix seconds, null when missing/unreadable */
  lastModified: number | null;
  /** profile name, empty for single-profile browsers */
  profile: string;
}

export interface PrivacyCategoryReport {
  id: PrivacyCategoryId;
  label: string;
  description: string;
  icon: string;
  bytes: number;
  items: number;
  targets: PrivacyTarget[];
}

export interface BrowserReport {
  id: BrowserId;
  label: string;
  icon: string;
  /** absolute path the catalog used to find this browser */
  root: string;
  /** false when catalog path doesn't exist on disk (not installed) */
  available: boolean;
  profiles: string[];
  bytes: number;
  items: number;
  categories: PrivacyCategoryReport[];
}

export interface PrivacyReport {
  totalBytes: number;
  totalItems: number;
  browsers: BrowserReport[];
  scannedAt: number;
  platform: 'mac' | 'linux' | 'windows';
  durationMs: number;
}

// sync privacy scan over host browsers. outside tauri returns a small
// mock so the screen renders during hmr
export function privacyScan(): Promise<PrivacyReport> {
  return invoke<PrivacyReport>('privacy_scan', undefined, mockPrivacyReport);
}

// flatten selected categories into absolute paths for the cleaner.
// callers pass the set of browser-id::category-id keys they've checked
export function selectedPathsFor(
  report: PrivacyReport,
  selected: Set<string>,
): string[] {
  const out: string[] = [];
  for (const b of report.browsers) {
    if (!b.available) continue;
    for (const c of b.categories) {
      if (!selected.has(`${b.id}::${c.id}`)) continue;
      for (const t of c.targets) out.push(t.path);
    }
  }
  return out;
}

function mockPrivacyReport(): PrivacyReport {
  const now = Math.floor(Date.now() / 1000);
  const MB = 1024 * 1024;
  return {
    totalBytes: 1860 * MB,
    totalItems: 18_412,
    scannedAt: now,
    platform: 'mac',
    durationMs: 420,
    browsers: [
      {
        id: 'chrome',
        label: 'Google Chrome',
        icon: 'shield',
        root: '~/Library/Application Support/Google/Chrome',
        available: true,
        profiles: ['Default', 'Profile 1'],
        bytes: 1_180 * MB,
        items: 12_418,
        categories: [
          {
            id: 'cache',
            label: 'Cache',
            description: 'HTTP, GPU, and code caches.',
            icon: 'broom',
            bytes: 820 * MB,
            items: 8_120,
            targets: [],
          },
          {
            id: 'cookies',
            label: 'Cookies',
            description: 'Site cookies.',
            icon: 'file',
            bytes: 2 * MB,
            items: 4,
            targets: [],
          },
          {
            id: 'history',
            label: 'Browsing history',
            description: 'Visited pages.',
            icon: 'archive',
            bytes: 14 * MB,
            items: 12,
            targets: [],
          },
          {
            id: 'sessions',
            label: 'Sessions',
            description: 'Tab restore data.',
            icon: 'file',
            bytes: 4 * MB,
            items: 8,
            targets: [],
          },
          {
            id: 'local-storage',
            label: 'Local storage',
            description: 'localStorage + IndexedDB.',
            icon: 'archive',
            bytes: 340 * MB,
            items: 4_274,
            targets: [],
          },
        ],
      },
      {
        id: 'firefox',
        label: 'Mozilla Firefox',
        icon: 'shield',
        root: '~/Library/Application Support/Firefox/Profiles',
        available: true,
        profiles: ['abc.default-release'],
        bytes: 610 * MB,
        items: 5_712,
        categories: [
          {
            id: 'cache',
            label: 'Cache',
            description: 'HTTP + startup cache.',
            icon: 'broom',
            bytes: 480 * MB,
            items: 5_500,
            targets: [],
          },
          {
            id: 'cookies',
            label: 'Cookies',
            description: 'Site cookies.',
            icon: 'file',
            bytes: 1 * MB,
            items: 3,
            targets: [],
          },
          {
            id: 'history',
            label: 'Browsing history',
            description: 'Places + favicons.',
            icon: 'archive',
            bytes: 20 * MB,
            items: 6,
            targets: [],
          },
          {
            id: 'sessions',
            label: 'Sessions',
            description: 'Session restore.',
            icon: 'file',
            bytes: 4 * MB,
            items: 3,
            targets: [],
          },
          {
            id: 'local-storage',
            label: 'Local storage',
            description: 'IndexedDB + site storage.',
            icon: 'archive',
            bytes: 105 * MB,
            items: 200,
            targets: [],
          },
        ],
      },
      {
        id: 'safari',
        label: 'Safari',
        icon: 'shield',
        root: '~/Library/Safari',
        available: true,
        profiles: [],
        bytes: 70 * MB,
        items: 282,
        categories: [
          {
            id: 'cache',
            label: 'Cache',
            description: 'WebKit caches.',
            icon: 'broom',
            bytes: 48 * MB,
            items: 260,
            targets: [],
          },
          {
            id: 'cookies',
            label: 'Cookies',
            description: 'Binary cookies store.',
            icon: 'file',
            bytes: 2 * MB,
            items: 2,
            targets: [],
          },
          {
            id: 'history',
            label: 'Browsing history',
            description: 'History.db.',
            icon: 'archive',
            bytes: 18 * MB,
            items: 5,
            targets: [],
          },
          {
            id: 'sessions',
            label: 'Sessions',
            description: 'LastSession.plist.',
            icon: 'file',
            bytes: 1 * MB,
            items: 2,
            targets: [],
          },
          {
            id: 'local-storage',
            label: 'Local storage',
            description: 'WebKit local storage.',
            icon: 'archive',
            bytes: 1 * MB,
            items: 13,
            targets: [],
          },
        ],
      },
      {
        id: 'edge',
        label: 'Microsoft Edge',
        icon: 'shield',
        root: '~/Library/Application Support/Microsoft Edge',
        available: false,
        profiles: [],
        bytes: 0,
        items: 0,
        categories: [
          { id: 'cache', label: 'Cache', description: '', icon: 'broom', bytes: 0, items: 0, targets: [] },
          { id: 'cookies', label: 'Cookies', description: '', icon: 'file', bytes: 0, items: 0, targets: [] },
          { id: 'history', label: 'Browsing history', description: '', icon: 'archive', bytes: 0, items: 0, targets: [] },
          { id: 'sessions', label: 'Sessions', description: '', icon: 'file', bytes: 0, items: 0, targets: [] },
          { id: 'local-storage', label: 'Local storage', description: '', icon: 'archive', bytes: 0, items: 0, targets: [] },
        ],
      },
    ],
  };
}
