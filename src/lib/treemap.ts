// ts mirror of src-tauri/src/scanner/treemap. wire format guarded by
// serializes_as_camelcase on the rust side

import { invoke, listen } from './ipc';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { createEnvelopeGate, type IpcEventEnvelope } from './events';

export interface TreemapRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface TreemapTile {
  key: string;
  name: string;
  path: string;
  bytes: number;
  fileCount: number;
  isDir: boolean;
  rect: TreemapRect;
  isOther: boolean;
}

export interface BiggestFolder {
  path: string;
  name: string;
  bytes: number;
  fileCount: number;
  depth: number;
}

export interface TreemapResponse {
  root: string;
  totalBytes: number;
  totalFiles: number;
  tiles: TreemapTile[];
  biggest: BiggestFolder[];
  scannedAt: number;
  durationMs: number;
}

export interface TreemapOptions {
  root?: string;
  depth?: number;
  maxTiles?: number;
}

// treemap for `root`. rust walks subtree, aggregates sizes up to
// `depth`, lays out top-level children as rects in the unit square so
// ui scales by the svg viewport.
// outside tauri returns a synthetic response so plain vite dev still
// renders, numbers are demo-only
export function computeTreemap(opts: TreemapOptions = {}): Promise<TreemapResponse> {
  const { root, depth, maxTiles } = opts;
  return invoke<TreemapResponse>(
    'compute_treemap',
    { root, depth, maxTiles },
    () => demoTreemap(root ?? '~'),
  );
}

function demoTreemap(root: string): TreemapResponse {
  // deterministic demo tiles so svg has something under vite dev.
  // shapes roughly match squarified output
  const demo: Array<{ name: string; bytes: number; rect: TreemapRect }> = [
    { name: 'node_modules', bytes: 52 * 1024 ** 3, rect: { x: 0, y: 0, w: 0.55, h: 1 } },
    { name: 'Photos', bytes: 28 * 1024 ** 3, rect: { x: 0.55, y: 0, w: 0.45, h: 0.55 } },
    { name: 'Videos', bytes: 14 * 1024 ** 3, rect: { x: 0.55, y: 0.55, w: 0.3, h: 0.45 } },
    { name: 'Downloads', bytes: 6 * 1024 ** 3, rect: { x: 0.85, y: 0.55, w: 0.15, h: 0.45 } },
  ];
  return {
    root,
    totalBytes: demo.reduce((s, d) => s + d.bytes, 0),
    totalFiles: 48_231,
    tiles: demo.map((d) => ({
      key: d.name,
      name: d.name,
      path: `${root}/${d.name}`,
      bytes: d.bytes,
      fileCount: 1000,
      isDir: true,
      rect: d.rect,
      isOther: false,
    })),
    biggest: demo.map((d, i) => ({
      path: `${root}/${d.name}`,
      name: d.name,
      bytes: d.bytes,
      fileCount: 1000,
      depth: 1 + (i % 2),
    })),
    scannedAt: Math.floor(Date.now() / 1000),
    durationMs: 0,
  };
}

// streaming variant, used by the ui

export interface TreemapHandle {
  id: string;
  root: string;
}

const CHANNEL_TREEMAP_PROGRESS = 'treemap://progress';
const CHANNEL_TREEMAP_DONE = 'treemap://done';

// start a streaming treemap walk. ui subscribes via subscribeTreemap.
// outside tauri returns a synthetic handle, no events emitted
export function startTreemap(opts: TreemapOptions = {}): Promise<TreemapHandle> {
  const { root, depth, maxTiles } = opts;
  return invoke<TreemapHandle>(
    'start_treemap',
    { root, depth, maxTiles },
    () => ({ id: `mock-${Math.random().toString(36).slice(2, 10)}`, root: root ?? '~' }),
  );
}

export function cancelTreemap(handleId: string): Promise<boolean> {
  return invoke<boolean>('cancel_treemap', { handleId }, () => false);
}

export function forgetTreemap(handleId: string): Promise<boolean> {
  return invoke<boolean>('forget_treemap', { handleId }, () => false);
}

export function treemapSnapshot(handleId: string): Promise<TreemapResponse | null> {
  return invoke<TreemapResponse | null>('treemap_snapshot', { handleId }, () => null);
}

// try to serve a treemap from the backend's in-memory cache without
// touching fs. hit = instant (clone + layout, ram-local). miss = null,
// caller falls back to startTreemap. used for drill-down + back-nav so
// user doesn't get charged a rescan for something already aggregated
export function serveTreemapSubtree(
  path?: string,
  maxTiles?: number,
): Promise<TreemapResponse | null> {
  return invoke<TreemapResponse | null>(
    'serve_treemap_subtree',
    { path, maxTiles },
    () => null,
  );
}

// drop every cached tree. rescan button calls this so next walk starts
// from bare fs state
export function invalidateTreemapCache(): Promise<void> {
  return invoke<void>('invalidate_treemap_cache', {}, () => undefined);
}

export interface TreemapSubscriptions {
  onProgress?: (resp: TreemapResponse) => void;
  onDone?: (resp: TreemapResponse) => void;
}

// subscribe to both treemap channels. returns one teardown. the shared
// envelope gate filters by handle id and drops stale sequence numbers.
export async function subscribeTreemap(
  handleId: string,
  subs: TreemapSubscriptions,
): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];
  const accept = createEnvelopeGate(handleId);
  if (subs.onProgress) {
    unlisteners.push(
      await listen<IpcEventEnvelope<TreemapResponse>>(CHANNEL_TREEMAP_PROGRESS, (ev) => {
        accept(ev, (payload) => subs.onProgress!(payload));
      }),
    );
  }
  if (subs.onDone) {
    unlisteners.push(
      await listen<IpcEventEnvelope<TreemapResponse>>(CHANNEL_TREEMAP_DONE, (ev) => {
        accept(ev, (payload) => subs.onDone!(payload));
      }),
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

// deterministic hue from tile name, same folder -> same color across
// renders. fnv-1a hash -> oklch so colors stay perceptually uniform
// on the dark bg
export function tileColor(name: string): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < name.length; i++) {
    h ^= name.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  const hue = Math.abs(h) % 360;
  return `oklch(0.62 0.12 ${hue})`;
}
