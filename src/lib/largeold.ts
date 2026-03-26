// ts mirror of src-tauri/src/scanner/largeold. wire format guarded by
// serializes_camelcase on the rust side

import { invoke, listen } from './ipc';
import type { UnlistenFn } from '@tauri-apps/api/event';

export interface FileSummary {
  path: string;
  bytes: number;
  /** unix seconds, null if mtime couldn't be read */
  modified: number | null;
  /** days since modified, floored */
  idleDays: number;
  /** lowercased ext, no dot, empty if none */
  extension: string;
}

export type ScanPhase = 'walking' | 'done';

export interface LargeOldReport {
  root: string;
  files: FileSummary[];
  totalMatched: number;
  totalBytes: number;
  totalFilesScanned: number;
  durationMs: number;
  phase: ScanPhase;
  minBytes: number;
  minDaysIdle: number;
}

export interface LargeOldOptions {
  root?: string;
  minBytes?: number;
  minDaysIdle?: number;
  maxResults?: number;
}

/** sync scan, streaming variant is the default */
export function findLargeOld(opts: LargeOldOptions = {}): Promise<LargeOldReport> {
  const { root, minBytes, minDaysIdle, maxResults } = opts;
  return invoke<LargeOldReport>(
    'find_large_old',
    { root, minBytes, minDaysIdle, maxResults },
    () => demoReport(root ?? '~'),
  );
}

export interface LargeOldHandle {
  id: string;
  root: string;
}

const CHANNEL_PROGRESS = 'large-old://progress';
const CHANNEL_DONE = 'large-old://done';

export function startLargeOld(opts: LargeOldOptions = {}): Promise<LargeOldHandle> {
  const { root, minBytes, minDaysIdle, maxResults } = opts;
  return invoke<LargeOldHandle>(
    'start_large_old',
    { root, minBytes, minDaysIdle, maxResults },
    () => ({ id: `mock-${Math.random().toString(36).slice(2, 10)}`, root: root ?? '~' }),
  );
}

export function cancelLargeOld(handleId: string): Promise<boolean> {
  return invoke<boolean>('cancel_large_old', { handleId }, () => false);
}

export function forgetLargeOld(handleId: string): Promise<boolean> {
  return invoke<boolean>('forget_large_old', { handleId }, () => false);
}

export function revealInFileManager(path: string): Promise<void> {
  return invoke<void>(
    'reveal_in_file_manager',
    { path },
    () => {
      // browser fallback, log so dev can verify wiring. real reveal
      // needs the tauri host
      // eslint-disable-next-line no-console
      console.log(`[safai] reveal requested (browser fallback): ${path}`);
    },
  );
}

export interface LargeOldSubscriptions {
  onProgress?: (resp: LargeOldReport) => void;
  onDone?: (resp: LargeOldReport) => void;
}

export async function subscribeLargeOld(
  subs: LargeOldSubscriptions,
): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];
  if (subs.onProgress) {
    unlisteners.push(
      await listen<LargeOldReport>(CHANNEL_PROGRESS, (r) => subs.onProgress!(r)),
    );
  }
  if (subs.onDone) {
    unlisteners.push(await listen<LargeOldReport>(CHANNEL_DONE, (r) => subs.onDone!(r)));
  }
  return () => {
    for (const u of unlisteners) {
      try {
        u();
      } catch {
        // best-effort
      }
    }
  };
}

export function phaseLabel(phase: ScanPhase): string {
  switch (phase) {
    case 'walking':
      return 'Walking the tree';
    case 'done':
      return 'Done';
  }
}

// mirrors rust DEFAULT_MIN_BYTES (50 MiB) + DEFAULT_MIN_DAYS_IDLE (180).
// keeping them in ts lets sliders show resolved values before the first
// scan returns
export const DEFAULT_MIN_BYTES = 50 * 1024 * 1024;
export const DEFAULT_MIN_DAYS_IDLE = 180;
export const DEFAULT_MAX_RESULTS = 1_000;

// extension -> color bucket so scatter points of the same flavour
// cluster visually. rest falls into "other". mirrors how finder/explorer
// categorise downloads
export type Bucket =
  | 'video'
  | 'image'
  | 'audio'
  | 'archive'
  | 'document'
  | 'installer'
  | 'disk-image'
  | 'data'
  | 'other';

const BUCKET_BY_EXT: Record<string, Bucket> = {
  mp4: 'video',
  mkv: 'video',
  mov: 'video',
  avi: 'video',
  webm: 'video',
  wmv: 'video',
  flv: 'video',
  m4v: 'video',
  jpg: 'image',
  jpeg: 'image',
  png: 'image',
  gif: 'image',
  tiff: 'image',
  tif: 'image',
  heic: 'image',
  raw: 'image',
  cr2: 'image',
  nef: 'image',
  arw: 'image',
  dng: 'image',
  psd: 'image',
  mp3: 'audio',
  wav: 'audio',
  flac: 'audio',
  m4a: 'audio',
  aac: 'audio',
  ogg: 'audio',
  aiff: 'audio',
  zip: 'archive',
  tar: 'archive',
  gz: 'archive',
  bz2: 'archive',
  xz: 'archive',
  '7z': 'archive',
  rar: 'archive',
  pdf: 'document',
  doc: 'document',
  docx: 'document',
  xls: 'document',
  xlsx: 'document',
  ppt: 'document',
  pptx: 'document',
  key: 'document',
  pages: 'document',
  numbers: 'document',
  epub: 'document',
  dmg: 'disk-image',
  iso: 'disk-image',
  vmdk: 'disk-image',
  vhd: 'disk-image',
  qcow2: 'disk-image',
  img: 'disk-image',
  pkg: 'installer',
  msi: 'installer',
  exe: 'installer',
  app: 'installer',
  deb: 'installer',
  rpm: 'installer',
  appimage: 'installer',
  sql: 'data',
  db: 'data',
  sqlite: 'data',
  csv: 'data',
  json: 'data',
  parquet: 'data',
  log: 'data',
};

export function bucketFor(ext: string): Bucket {
  return BUCKET_BY_EXT[ext] ?? 'other';
}

// stable color per bucket from the design palette so scatter blends
// with rest of ui
export function bucketColour(bucket: Bucket): string {
  switch (bucket) {
    case 'video':
      return 'oklch(0.72 0.18 305)'; // magenta
    case 'image':
      return 'oklch(0.82 0.14 200)'; // cyan
    case 'audio':
      return 'oklch(0.78 0.16 145)'; // mint
    case 'archive':
      return 'oklch(0.74 0.14 85)';  // amber
    case 'document':
      return 'oklch(0.82 0.14 230)'; // blue
    case 'installer':
      return 'oklch(0.68 0.18 25)';  // coral
    case 'disk-image':
      return 'oklch(0.78 0.12 50)';  // orange
    case 'data':
      return 'oklch(0.74 0.14 160)'; // teal
    case 'other':
      return 'oklch(0.60 0.02 240)'; // grey
  }
}

export function bucketLabel(bucket: Bucket): string {
  switch (bucket) {
    case 'disk-image':
      return 'Disk image';
    default:
      return bucket.charAt(0).toUpperCase() + bucket.slice(1);
  }
}

// mocks for plain-browser dev

function demoReport(root: string): LargeOldReport {
  const now = Math.floor(Date.now() / 1000);
  const files: FileSummary[] = [
    {
      path: `${root}/Downloads/Ubuntu-22.04.iso`,
      bytes: 3.8 * 1024 * 1024 * 1024,
      modified: now - 86400 * 320,
      idleDays: 320,
      extension: 'iso',
    },
    {
      path: `${root}/Movies/trip-2023-raw.mp4`,
      bytes: 2.1 * 1024 * 1024 * 1024,
      modified: now - 86400 * 412,
      idleDays: 412,
      extension: 'mp4',
    },
    {
      path: `${root}/Documents/old-backup.zip`,
      bytes: 780 * 1024 * 1024,
      modified: now - 86400 * 600,
      idleDays: 600,
      extension: 'zip',
    },
    {
      path: `${root}/Pictures/wedding-edits.psd`,
      bytes: 520 * 1024 * 1024,
      modified: now - 86400 * 240,
      idleDays: 240,
      extension: 'psd',
    },
    {
      path: `${root}/Downloads/macOS-Ventura.dmg`,
      bytes: 12.2 * 1024 * 1024 * 1024,
      modified: now - 86400 * 180,
      idleDays: 180,
      extension: 'dmg',
    },
  ];
  return {
    root,
    files,
    totalMatched: files.length,
    totalBytes: files.reduce((a, b) => a + b.bytes, 0),
    totalFilesScanned: 48_210,
    durationMs: 540,
    phase: 'done',
    minBytes: DEFAULT_MIN_BYTES,
    minDaysIdle: DEFAULT_MIN_DAYS_IDLE,
  };
}
