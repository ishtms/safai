// ts mirror of src-tauri/src/scanner. two surfaces:
// * smart_scan_summary - dashboard roll-up from last completed scan
//   (rust keeps it in LastScanStore)
// * streaming walker control (start_scan/cancel_scan/...) + event
//   payloads on scan://event|progress|done
// rust tests serialization_uses_camelcase_and_kebab_enum +
// serializes_ids_as_kebab_case guard the wire format

import { invoke, listen } from './ipc';
import type { UnlistenFn } from '@tauri-apps/api/event';

export type CategoryId =
  | 'system-junk'
  | 'duplicates'
  | 'large-old'
  | 'privacy'
  | 'app-leftovers'
  | 'trash';

export interface CategorySummary {
  id: CategoryId;
  label: string;
  icon: string;
  colorVar: string;
  bytes: number;
  items: number;
  safeNote: string;
}

export interface SmartScanSummary {
  totalBytes: number;
  totalItems: number;
  scannedAt: number | null;
  categories: CategorySummary[];
  mocked: boolean;
}

// smart scan dashboard roll-up. empty fallback outside tauri so ui can
// render a skeleton instead of hanging
export function fetchSmartScanSummary(): Promise<SmartScanSummary> {
  return invoke<SmartScanSummary>('smart_scan_summary', undefined, () => ({
    totalBytes: 0,
    totalItems: 0,
    scannedAt: null,
    categories: [],
    mocked: true,
  }));
}

// streaming scan

export type ScanEventKind = 'scan' | 'found' | 'safe';
export type ScanState = 'running' | 'paused' | 'cancelled' | 'done';

export interface ScanEvent {
  handleId: string;
  kind: ScanEventKind;
  path: string;
  bytes: number;
  elapsedMs: number;
}

export interface ScanProgress {
  filesScanned: number;
  bytesScanned: number;
  flaggedBytes: number;
  flaggedItems: number;
  elapsedMs: number;
  state: ScanState;
  currentPath: string | null;
}

export interface ScanHandle {
  id: string;
  roots: string[];
}

// must match commands.rs::EVENT_SCAN_*
const CHANNEL_EVENT = 'scan://event';
const CHANNEL_PROGRESS = 'scan://progress';
const CHANNEL_DONE = 'scan://done';

// start a streaming scan. handle lets caller subscribe + pause/resume/
// cancel. roots omitted -> rust defaults to $HOME.
// outside tauri returns a synthetic handle and emits nothing so the
// /scanning screen still shows its waiting state
export function startScan(roots?: string[]): Promise<ScanHandle> {
  return invoke<ScanHandle>('start_scan', { roots }, () => ({
    id: `mock-${Math.random().toString(36).slice(2, 10)}`,
    roots: roots ?? ['~'],
  }));
}

export function cancelScan(handleId: string): Promise<boolean> {
  return invoke<boolean>('cancel_scan', { handleId }, () => false);
}

export function pauseScan(handleId: string): Promise<boolean> {
  return invoke<boolean>('pause_scan', { handleId }, () => false);
}

export function resumeScan(handleId: string): Promise<boolean> {
  return invoke<boolean>('resume_scan', { handleId }, () => false);
}

export function scanSnapshot(handleId: string): Promise<ScanProgress | null> {
  return invoke<ScanProgress | null>('scan_snapshot', { handleId }, () => null);
}

export function forgetScan(handleId: string): Promise<boolean> {
  return invoke<boolean>('forget_scan', { handleId }, () => false);
}

export interface ScanSubscriptions {
  onEvent?: (ev: ScanEvent) => void;
  onProgress?: (p: ScanProgress) => void;
  onDone?: (p: ScanProgress) => void;
}

// subscribe to all three scan channels for one handle. events for
// other handles get filtered so concurrent scans stay isolated.
// returns an unlisten that tears down every subscription
export async function subscribeScan(
  handleId: string,
  subs: ScanSubscriptions,
): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];
  if (subs.onEvent) {
    unlisteners.push(
      await listen<ScanEvent>(CHANNEL_EVENT, (ev) => {
        if (ev.handleId === handleId) subs.onEvent!(ev);
      }),
    );
  }
  if (subs.onProgress) {
    unlisteners.push(
      await listen<ScanProgress>(CHANNEL_PROGRESS, (p) => subs.onProgress!(p)),
    );
  }
  if (subs.onDone) {
    unlisteners.push(
      await listen<ScanProgress>(CHANNEL_DONE, (p) => subs.onDone!(p)),
    );
  }
  return () => {
    for (const u of unlisteners) {
      try {
        u();
      } catch {
        // best-effort during teardown
      }
    }
  };
}
