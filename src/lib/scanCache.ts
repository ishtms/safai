// module-level scan cache. solid router remounts screens on nav, so a naive
// `createResource(junkScan)` inside the component re-runs the scan every
// tab switch. this wraps createResource inside a createRoot so the signal
// lives at module scope, survives unmount, and fires the fetcher once
// until someone calls refetch().
//
// usage from a screen:
//   const [report, { refetch }] = sharedResource('junk', junkScan);
//
// the returned tuple is the same shape solid's createResource exposes, so
// screens swap in with a one-line change. `refetch()` forces a fresh fetch
// (rescan button), otherwise the cached value persists until invalidate().

import {
  createResource,
  createRoot,
  type Resource,
  type ResourceActions,
} from 'solid-js';

interface Entry<T> {
  resource: Resource<T>;
  actions: ResourceActions<T | undefined>;
  dispose: () => void;
  descriptor: NormalizedScanCacheDescriptor;
  metadata: ScanCacheMetadata | null;
  updatedAtMs: number | null;
}

export const DEFAULT_CACHE_STALE_AFTER_MS = 15 * 60 * 1000;

export interface ScanCacheDescriptor {
  key: string;
  rootPath?: string | null;
  options?: Record<string, unknown>;
  staleAfterMs?: number;
}

export type ScanCacheKey = string | ScanCacheDescriptor;

export interface ScanCacheMetadata {
  key: string;
  cachedAtMs: number;
  scannedAtMs: number;
  rootPath: string | null;
  options: Record<string, unknown>;
  staleAfterMs: number;
}

export interface ScanCacheFreshness {
  metadata: ScanCacheMetadata | null;
  ageMs: number | null;
  stale: boolean;
}

interface NormalizedScanCacheDescriptor {
  key: string;
  rootPath: string | null;
  options: Record<string, unknown>;
  staleAfterMs: number;
}

const store: Map<string, Entry<unknown>> = new Map();

// shared resource, keyed by string. multiple callers for the same key reuse
// the same signal + fetch. fetcher is captured on first call, later callers
// with different fetchers still get the cached value - rescan via refetch()
// or drop via invalidate() if you need a new fetcher
export function sharedResource<T>(
  cacheKey: ScanCacheKey,
  fetcher: () => Promise<T>,
): [Resource<T>, ResourceActions<T | undefined>] {
  const descriptor = normalizeDescriptor(cacheKey);
  const hit = store.get(descriptor.key) as Entry<T> | undefined;
  if (hit) {
    hit.descriptor = mergeDescriptors(hit.descriptor, descriptor);
    return [hit.resource, hit.actions];
  }
  let entry!: Entry<T>;
  const wrappedFetcher = async () => {
    const value = await fetcher();
    if (entry) recordCacheValue(entry, descriptor, value);
    return value;
  };
  let resource!: Resource<T>;
  let actions!: ResourceActions<T | undefined>;
  const dispose = createRoot((d) => {
    const [r, a] = createResource<T>(wrappedFetcher);
    resource = r;
    actions = a;
    return d;
  });
  entry = {
    resource,
    actions,
    dispose,
    descriptor,
    metadata: null,
    updatedAtMs: null,
  };
  store.set(descriptor.key, entry as Entry<unknown>);
  return [resource, actions];
}

// drop the cache for `key`. next sharedResource() call refires the fetcher.
// use when the underlying fetcher closure needs to change (e.g. different
// args), otherwise prefer refetch()
export function invalidate(cacheKey: ScanCacheKey): void {
  const key = cacheKeyOf(cacheKey);
  const hit = store.get(key);
  if (!hit) return;
  hit.dispose();
  store.delete(key);
}

export function invalidatePrefix(prefix: string): void {
  const keys = Array.from(store.keys()).filter(
    (key) => key === prefix || key.startsWith(`${prefix}:`),
  );
  for (const key of keys) invalidate(key);
}

export function invalidateFilesystemScanCaches(except?: string | string[]): void {
  const skipped = new Set(Array.isArray(except) ? except : except ? [except] : []);
  for (const prefix of FILESYSTEM_SCAN_CACHE_PREFIXES) {
    if (skipped.has(prefix)) continue;
    invalidatePrefix(prefix);
  }
}

// forces the cached resource to re-run its fetcher, returns the new
// promise. use for rescan buttons. returns undefined if the key isn't in
// the cache yet
export function refetch<T>(cacheKey: ScanCacheKey): Promise<T | undefined> | undefined {
  const key = cacheKeyOf(cacheKey);
  const hit = store.get(key) as Entry<T> | undefined;
  if (!hit) return undefined;
  return hit.actions.refetch() as Promise<T | undefined>;
}

// mutate the cached value directly. used by streaming scans (malware,
// large-old, dupes) where the fetcher stays idle and the real updates
// come from tauri event streams - set the final report once done so
// future mounts read the cached copy instead of kicking off a new scan
export function setCached<T>(cacheKey: ScanCacheKey, value: T): void {
  const descriptor = normalizeDescriptor(cacheKey);
  const key = descriptor.key;
  const hit = store.get(key) as Entry<T> | undefined;
  if (!hit) {
    // no resource yet, stash a no-op fetcher that resolves to value so
    // downstream sharedResource() calls get the cached data immediately
    const [, actions] = sharedResource<T>(descriptor, async () => value);
    const created = store.get(key) as Entry<T> | undefined;
    if (created) recordCacheValue(created, descriptor, value);
    (actions.mutate as (v: T) => void)(value);
    return;
  }
  recordCacheValue(hit, descriptor, value);
  // cast through unknown: mutate's inferred signature forbids bare T for
  // generics when T could structurally match a function, but at runtime
  // we want a plain replace
  (hit.actions.mutate as (v: T) => void)(value);
}

// peek at a cached value without creating a resource. returns undefined
// when nothing is cached yet. used by streaming screens to decide whether
// to kick off a fresh scan or just render the previous one
export function peekCached<T>(cacheKey: ScanCacheKey): T | undefined {
  const key = cacheKeyOf(cacheKey);
  const hit = store.get(key) as Entry<T> | undefined;
  if (!hit) return undefined;
  return hit.resource();
}

export function cacheUpdatedAt(cacheKey: ScanCacheKey): number | null {
  const hit = store.get(cacheKeyOf(cacheKey));
  return hit?.metadata?.cachedAtMs ?? hit?.updatedAtMs ?? null;
}

export function cacheMetadata(cacheKey: ScanCacheKey): ScanCacheMetadata | null {
  return store.get(cacheKeyOf(cacheKey))?.metadata ?? null;
}

export function cacheFreshness(
  cacheKey: ScanCacheKey,
  nowMs: number = Date.now(),
): ScanCacheFreshness {
  const metadata = cacheMetadata(cacheKey);
  if (!metadata) return { metadata: null, ageMs: null, stale: false };
  const ageMs = Math.max(0, nowMs - metadata.cachedAtMs);
  return {
    metadata,
    ageMs,
    stale: ageMs > metadata.staleAfterMs,
  };
}

// clear every cached resource. used on settings "reset" / graveyard purge
// when the user wants a clean slate
export function clearAll(): void {
  for (const entry of store.values()) {
    entry.dispose();
  }
  store.clear();
}

// well-known cache keys. keep here so screens can't typo them
export const KEY_JUNK = 'scan:junk';
export const KEY_PRIVACY = 'scan:privacy';
export const KEY_STARTUP = 'scan:startup';
export const KEY_DUPLICATES = 'scan:duplicates';
export const KEY_LARGE_OLD = 'scan:largeold';
export const KEY_MALWARE = 'scan:malware';
export const KEY_TREEMAP = 'scan:treemap';

export const CACHE_JUNK = scanCacheDescriptor(
  KEY_JUNK,
  { scan: 'default-catalog' },
  { rootPath: 'Junk catalog roots' },
);
export const CACHE_PRIVACY = scanCacheDescriptor(
  KEY_PRIVACY,
  { scan: 'browser-cleanup-catalog' },
  { rootPath: 'Browser profile roots' },
);
export const CACHE_STARTUP = scanCacheDescriptor(
  KEY_STARTUP,
  { scan: 'login-items' },
  { rootPath: 'Login item locations' },
);

const FILESYSTEM_SCAN_CACHE_PREFIXES = [
  KEY_JUNK,
  KEY_PRIVACY,
  KEY_DUPLICATES,
  KEY_LARGE_OLD,
  KEY_MALWARE,
  KEY_TREEMAP,
];

export function scanCacheKey(
  base: string,
  options: Record<string, unknown> = {},
): string {
  const suffix = stableStringify(options);
  return suffix === '{}' ? base : `${base}:${suffix}`;
}

export function scanCacheDescriptor(
  base: string,
  options: Record<string, unknown> = {},
  metadata: Pick<ScanCacheDescriptor, 'rootPath' | 'staleAfterMs'> = {},
): ScanCacheDescriptor {
  return {
    key: scanCacheKey(base, options),
    options: cleanOptions(options),
    rootPath: metadata.rootPath ?? null,
    staleAfterMs: metadata.staleAfterMs,
  };
}

function cacheKeyOf(cacheKey: ScanCacheKey): string {
  return typeof cacheKey === 'string' ? cacheKey : cacheKey.key;
}

function normalizeDescriptor(cacheKey: ScanCacheKey): NormalizedScanCacheDescriptor {
  if (typeof cacheKey === 'string') {
    return {
      key: cacheKey,
      rootPath: null,
      options: {},
      staleAfterMs: DEFAULT_CACHE_STALE_AFTER_MS,
    };
  }
  return {
    key: cacheKey.key,
    rootPath: cacheKey.rootPath ?? null,
    options: cleanOptions(cacheKey.options ?? {}),
    staleAfterMs: cacheKey.staleAfterMs ?? DEFAULT_CACHE_STALE_AFTER_MS,
  };
}

function mergeDescriptors(
  current: NormalizedScanCacheDescriptor,
  incoming: NormalizedScanCacheDescriptor,
): NormalizedScanCacheDescriptor {
  return {
    key: current.key,
    rootPath: incoming.rootPath ?? current.rootPath,
    options:
      Object.keys(incoming.options).length > 0 ? incoming.options : current.options,
    staleAfterMs: incoming.staleAfterMs,
  };
}

function recordCacheValue<T>(
  entry: Entry<T>,
  descriptor: NormalizedScanCacheDescriptor,
  value: T,
): void {
  entry.descriptor = mergeDescriptors(entry.descriptor, descriptor);
  const cachedAtMs = Date.now();
  entry.updatedAtMs = cachedAtMs;
  entry.metadata = metadataForValue(entry.descriptor, value, cachedAtMs);
}

function metadataForValue<T>(
  descriptor: NormalizedScanCacheDescriptor,
  value: T,
  cachedAtMs: number,
): ScanCacheMetadata {
  return {
    key: descriptor.key,
    cachedAtMs,
    scannedAtMs: readScannedAtMs(value) ?? cachedAtMs,
    rootPath: descriptor.rootPath ?? readRootPath(value),
    options: { ...descriptor.options },
    staleAfterMs: descriptor.staleAfterMs,
  };
}

function readScannedAtMs(value: unknown): number | null {
  if (!value || typeof value !== 'object') return null;
  const scannedAt = (value as { scannedAt?: unknown }).scannedAt;
  if (typeof scannedAt !== 'number' || !Number.isFinite(scannedAt)) return null;
  return scannedAt > 10_000_000_000 ? scannedAt : scannedAt * 1000;
}

function readRootPath(value: unknown): string | null {
  if (!value || typeof value !== 'object') return null;
  const root = (value as { root?: unknown }).root;
  return typeof root === 'string' && root.length > 0 ? root : null;
}

function cleanOptions(options: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const key of Object.keys(options).sort()) {
    const value = options[key];
    if (value !== undefined) out[key] = value;
  }
  return out;
}

function stableStringify(value: unknown): string {
  if (value == null || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(',')}]`;
  const obj = value as Record<string, unknown>;
  const entries = Object.keys(obj)
    .filter((key) => obj[key] !== undefined)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${stableStringify(obj[key])}`);
  return `{${entries.join(',')}}`;
}
