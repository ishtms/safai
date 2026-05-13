// ts mirror of src-tauri/src/scanner/dupes. wire format guarded by
// serializes_as_camelcase on the rust side

import { invoke, listen } from './ipc';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { createEnvelopeGate, type IpcEventEnvelope } from './events';

export interface DuplicateFile {
  path: string;
  bytes: number;
  /** unix seconds, null if mtime wasn't readable */
  modified: number | null;
}

export interface DuplicateGroup {
  id: string;
  hash: string;
  bytesEach: number;
  files: DuplicateFile[];
  wastedBytes: number;
}

export type ScanPhase = 'walking' | 'size-grouped' | 'head-hashed' | 'done';

export interface DuplicateReport {
  root: string;
  groups: DuplicateGroup[];
  /** regular files seen during the walk; small files can be counted here then skipped */
  totalFilesScanned: number;
  totalGroups: number;
  wastedBytes: number;
  durationMs: number;
  /** current phase, `done` means groups are final */
  phase: ScanPhase;
  /** eligible files while walking, then files still in play for the next phase */
  candidatesRemaining: number;
}

export interface DuplicateOptions {
  root?: string;
  /** files smaller than this get skipped. rust default is 1 MiB */
  minBytes?: number;
}

export const DEFAULT_MIN_BYTES = 1024 * 1024;

// sync scan, rarely used from ui. streaming variant is the default so
// the ui shows progress instead of spinning
export function findDuplicates(opts: DuplicateOptions = {}): Promise<DuplicateReport> {
  const { root, minBytes } = opts;
  return invoke<DuplicateReport>(
    'find_duplicates',
    { root, minBytes },
    () => demoReport(root ?? '~'),
  );
}

export interface DuplicatesHandle {
  id: string;
  root: string;
}

const CHANNEL_DUPES_PROGRESS = 'dupes://progress';
const CHANNEL_DUPES_DONE = 'dupes://done';

export function startDuplicates(opts: DuplicateOptions = {}): Promise<DuplicatesHandle> {
  const { root, minBytes } = opts;
  return invoke<DuplicatesHandle>(
    'start_duplicates',
    { root, minBytes },
    () => ({ id: `mock-${Math.random().toString(36).slice(2, 10)}`, root: root ?? '~' }),
  );
}

export function cancelDuplicates(handleId: string): Promise<boolean> {
  return invoke<boolean>('cancel_duplicates', { handleId }, () => false);
}

export function forgetDuplicates(handleId: string): Promise<boolean> {
  return invoke<boolean>('forget_duplicates', { handleId }, () => false);
}

export function duplicatesSnapshot(handleId: string): Promise<DuplicateReport | null> {
  return invoke<DuplicateReport | null>('duplicates_snapshot', { handleId }, () => null);
}

export interface DuplicatesSubscriptions {
  onProgress?: (resp: DuplicateReport) => void;
  onDone?: (resp: DuplicateReport) => void;
}

export async function subscribeDuplicates(
  handleId: string,
  subs: DuplicatesSubscriptions,
): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];
  const accept = createEnvelopeGate(handleId);
  if (subs.onProgress) {
    unlisteners.push(
      await listen<IpcEventEnvelope<DuplicateReport>>(CHANNEL_DUPES_PROGRESS, (ev) => {
        accept(ev, (payload) => subs.onProgress!(payload));
      }),
    );
  }
  if (subs.onDone) {
    unlisteners.push(
      await listen<IpcEventEnvelope<DuplicateReport>>(CHANNEL_DUPES_DONE, (ev) => {
        accept(ev, (payload) => subs.onDone!(payload));
      }),
    );
  }
  return () => {
    for (const u of unlisteners) {
      try {
        u();
      } catch {
        // best-effort teardown
      }
    }
  };
}

// mocks for plain-browser dev

function demoReport(root: string): DuplicateReport {
  const now = Math.floor(Date.now() / 1000);
  const groups: DuplicateGroup[] = [
    {
      id: 'demo-img-set',
      hash: 'demo-img-set'.padEnd(64, '0'),
      bytesEach: 12 * 1024 * 1024,
      wastedBytes: 24 * 1024 * 1024,
      files: [
        {
          path: `${root}/Pictures/summer_trip.jpg`,
          bytes: 12 * 1024 * 1024,
          modified: now - 86400 * 30,
        },
        {
          path: `${root}/Pictures/Copy of summer_trip.jpg`,
          bytes: 12 * 1024 * 1024,
          modified: now - 86400 * 5,
        },
        {
          path: `${root}/Desktop/summer_trip.jpg`,
          bytes: 12 * 1024 * 1024,
          modified: now - 86400 * 1,
        },
      ],
    },
    {
      id: 'demo-doc',
      hash: 'demo-doc'.padEnd(64, '0'),
      bytesEach: 2 * 1024 * 1024,
      wastedBytes: 2 * 1024 * 1024,
      files: [
        { path: `${root}/Downloads/invoice.pdf`, bytes: 2 * 1024 * 1024, modified: now - 3600 },
        {
          path: `${root}/Documents/invoice.pdf`,
          bytes: 2 * 1024 * 1024,
          modified: now - 7200,
        },
      ],
    },
  ];
  return {
    root,
    groups,
    totalFilesScanned: 14_203,
    totalGroups: groups.length,
    wastedBytes: groups.reduce((a, b) => a + b.wastedBytes, 0),
    durationMs: 820,
    phase: 'done',
    candidatesRemaining: groups.length,
  };
}

export function phaseLabel(phase: ScanPhase): string {
  switch (phase) {
    case 'walking':
      return 'Walking the tree';
    case 'size-grouped':
      return 'Comparing sizes';
    case 'head-hashed':
      return 'Hashing full contents';
    case 'done':
      return 'Done';
  }
}

// given a group + keep set, return paths to delete. screen uses this
// to feed the cleaner
export function pathsToDelete(
  group: DuplicateGroup,
  kept: Set<string>,
): string[] {
  return group.files.filter((f) => !kept.has(f.path)).map((f) => f.path);
}

// auto-pick which file to keep per group. oldest by mtime, tie-break
// by shortest path (least copy-of-ish). ui exposes this as
// "select copies, keep originals"
export function autoKeepOriginal(group: DuplicateGroup): string {
  // reduce instead of sort, stays O(n) on huge groups
  return group.files.reduce((best, f) => {
    const bestMod = best.modified ?? Number.POSITIVE_INFINITY;
    const fMod = f.modified ?? Number.POSITIVE_INFINITY;
    if (fMod < bestMod) return f;
    if (fMod > bestMod) return best;
    return f.path.length < best.path.length ? f : best;
  }).path;
}
