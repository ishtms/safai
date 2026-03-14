// ts mirror of src-tauri/src/cleaner. wire format tracks rust
// camelCase structs + kebab ItemKind variants. rust tests
// wire_format_is_stable_camel_case + item_kind_uses_kebab_case guard
// the contract, rename fields on both sides or break the wire

import { invoke } from './ipc';

export type ItemKind = 'file' | 'directory' | 'symlink' | 'missing';

export interface PendingDelete {
  path: string;
  bytes: number;
  fileCount: number;
  kind: ItemKind;
  /** true when safety policy refuses to trash this */
  protected: boolean;
  /** short reason, when protected is true */
  protectedReason: string | null;
}

export interface DeletePlan {
  /** opaque handle for commitDelete, don't resubmit raw paths */
  token: string;
  createdAt: number;
  items: PendingDelete[];
  /** bytes across non-protected items only */
  totalBytes: number;
  /** count of non-protected items, what'll actually move */
  totalCount: number;
  /** items cleaner refused, show as "X skipped" in the modal */
  protectedCount: number;
}

export interface DeleteFailure {
  path: string;
  error: string;
}

export interface DeleteResult {
  token: string;
  batchId: string;
  committedAt: number;
  committed: string[];
  failed: DeleteFailure[];
  bytesTrashed: number;
}

export interface RestoreResult {
  batchId: string;
  restoredAt: number;
  restored: string[];
  failed: DeleteFailure[];
  bytesRestored: number;
}

export interface PurgeResult {
  purged: string[];
  failed: DeleteFailure[];
  bytesFreed: number;
  purgedAt: number;
}

export interface GraveyardStats {
  batchCount: number;
  totalBytes: number;
  /** unix seconds, null when empty */
  oldestAt: number | null;
  newestAt: number | null;
}

// build a plan the user confirms before anything moves. token goes
// into commitDelete, never resubmit raw paths
export function previewDelete(paths: string[]): Promise<DeletePlan> {
  return invoke<DeletePlan>('preview_delete', { paths }, () => mockPlan(paths));
}

export function commitDelete(token: string): Promise<DeleteResult> {
  return invoke<DeleteResult>('commit_delete', { token }, () => mockCommit(token));
}

export function restoreLast(): Promise<RestoreResult> {
  return invoke<RestoreResult>('restore_last', undefined, mockRestore);
}

export function graveyardStats(): Promise<GraveyardStats> {
  return invoke<GraveyardStats>('graveyard_stats', undefined, () => ({
    batchCount: 0,
    totalBytes: 0,
    oldestAt: null,
    newestAt: null,
  }));
}

export function purgeGraveyard(): Promise<PurgeResult> {
  return invoke<PurgeResult>('purge_graveyard', undefined, () => ({
    purged: [],
    failed: [],
    bytesFreed: 0,
    purgedAt: Math.floor(Date.now() / 1000),
  }));
}

// mocks for plain-browser dev (pnpm dev without tauri dev). lets
// Junk.tsx render the confirm modal + fake success during hmr so we
// can iterate without firing rust. never runs in prod

function mockPlan(paths: string[]): DeletePlan {
  const items: PendingDelete[] = paths.map((p, i) => ({
    path: p,
    bytes: (i + 1) * 1024 * 1024,
    fileCount: (i + 1) * 10,
    kind: 'directory',
    protected: false,
    protectedReason: null,
  }));
  return {
    token: `plan-mock-${Date.now().toString(16)}`,
    createdAt: Math.floor(Date.now() / 1000),
    items,
    totalBytes: items.reduce((a, b) => a + b.bytes, 0),
    totalCount: items.length,
    protectedCount: 0,
  };
}

function mockCommit(token: string): DeleteResult {
  return {
    token,
    batchId: `b-mock-${Date.now().toString(16)}`,
    committedAt: Math.floor(Date.now() / 1000),
    committed: [],
    failed: [],
    bytesTrashed: 0,
  };
}

function mockRestore(): RestoreResult {
  return {
    batchId: `b-mock-restore`,
    restoredAt: Math.floor(Date.now() / 1000),
    restored: [],
    failed: [],
    bytesRestored: 0,
  };
}
