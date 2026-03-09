// ts mirror of src-tauri/src/scanner/junk. camelCase structs, kebab
// JunkCategoryId variants. rust tests id_str_round_trip_is_kebab +
// serialization_is_camelcase_with_kebab_ids guard the contract, rename
// fields on both sides or break the wire

import { invoke } from './ipc';

export type JunkCategoryId =
  | 'user-caches'
  | 'system-logs'
  | 'xcode-derived-data'
  | 'npm-cache'
  | 'pnpm-store'
  | 'cargo-cache'
  | 'go-mod-cache'
  | 'trash'
  | 'temp-files'
  | 'chrome-cache'
  | 'edge-cache'
  | 'firefox-cache';

export interface JunkPathDetail {
  path: string;
  bytes: number;
  fileCount: number;
  /** unix seconds, null for empty/unreadable subtrees */
  lastModified: number | null;
}

export interface JunkCategoryReport {
  id: JunkCategoryId;
  label: string;
  description: string;
  icon: string;
  hot: boolean;
  bytes: number;
  items: number;
  /** false when no catalog base exists on this host */
  available: boolean;
  paths: JunkPathDetail[];
}

export interface JunkReport {
  totalBytes: number;
  totalItems: number;
  categories: JunkCategoryReport[];
  scannedAt: number;
  platform: 'mac' | 'linux' | 'windows';
  durationMs: number;
}

// sync system junk scan. outside tauri returns a small mock so the
// screen renders a populated layout during hmr
export function junkScan(): Promise<JunkReport> {
  return invoke<JunkReport>('junk_scan', undefined, mockJunkReport);
}

function mockJunkReport(): JunkReport {
  const now = Math.floor(Date.now() / 1000);
  const MB = 1024 * 1024;
  return {
    totalBytes: 4200 * MB,
    totalItems: 12_847,
    scannedAt: now,
    platform: 'mac',
    durationMs: 820,
    categories: [
      {
        id: 'user-caches',
        label: 'User caches',
        description: 'Per-app caches. Apps regenerate these on next launch.',
        icon: 'broom',
        hot: false,
        bytes: 2_150 * MB,
        items: 8_431,
        available: true,
        paths: [
          { path: '~/Library/Caches/Google/Chrome', bytes: 1_240 * MB, fileCount: 4_218, lastModified: now - 86_400 * 14 },
          { path: '~/Library/Caches/Spotify', bytes: 412 * MB, fileCount: 1_903, lastModified: now - 86_400 * 3 },
          { path: '~/Library/Caches/Slack', bytes: 287 * MB, fileCount: 842, lastModified: now - 86_400 },
          { path: '~/Library/Caches/com.figma.Desktop', bytes: 184 * MB, fileCount: 318, lastModified: now - 3600 * 6 },
        ],
      },
      {
        id: 'system-logs',
        label: 'System logs',
        description: 'User-scope app logs. Rotate on their own.',
        icon: 'file',
        hot: false,
        bytes: 892 * MB,
        items: 2_103,
        available: true,
        paths: [],
      },
      {
        id: 'xcode-derived-data',
        label: 'Xcode derived data',
        description: 'Build products Xcode regenerates on the next build.',
        icon: 'archive',
        hot: true,
        bytes: 842 * MB,
        items: 1_872,
        available: true,
        paths: [],
      },
    ],
  };
}
