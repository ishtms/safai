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
}

const store: Map<string, Entry<unknown>> = new Map();

// shared resource, keyed by string. multiple callers for the same key reuse
// the same signal + fetch. fetcher is captured on first call, later callers
// with different fetchers still get the cached value - rescan via refetch()
// or drop via invalidate() if you need a new fetcher
export function sharedResource<T>(
  key: string,
  fetcher: () => Promise<T>,
): [Resource<T>, ResourceActions<T | undefined>] {
  const hit = store.get(key) as Entry<T> | undefined;
  if (hit) {
    return [hit.resource, hit.actions];
  }
  let resource!: Resource<T>;
  let actions!: ResourceActions<T | undefined>;
  const dispose = createRoot((d) => {
    const [r, a] = createResource<T>(fetcher);
    resource = r;
    actions = a;
    return d;
  });
  const entry: Entry<T> = { resource, actions, dispose };
  store.set(key, entry as Entry<unknown>);
  return [resource, actions];
}

// drop the cache for `key`. next sharedResource() call refires the fetcher.
// use when the underlying fetcher closure needs to change (e.g. different
// args), otherwise prefer refetch()
export function invalidate(key: string): void {
  const hit = store.get(key);
  if (!hit) return;
  hit.dispose();
  store.delete(key);
}

// forces the cached resource to re-run its fetcher, returns the new
// promise. use for rescan buttons. returns undefined if the key isn't in
// the cache yet
export function refetch<T>(key: string): Promise<T | undefined> | undefined {
  const hit = store.get(key) as Entry<T> | undefined;
  if (!hit) return undefined;
  return hit.actions.refetch() as Promise<T | undefined>;
}

// mutate the cached value directly. used by streaming scans (malware,
// large-old, dupes) where the fetcher stays idle and the real updates
// come from tauri event streams - set the final report once done so
// future mounts read the cached copy instead of kicking off a new scan
export function setCached<T>(key: string, value: T): void {
  const hit = store.get(key) as Entry<T> | undefined;
  if (!hit) {
    // no resource yet, stash a no-op fetcher that resolves to value so
    // downstream sharedResource() calls get the cached data immediately
    sharedResource<T>(key, async () => value);
    return;
  }
  // cast through unknown: mutate's inferred signature forbids bare T for
  // generics when T could structurally match a function, but at runtime
  // we want a plain replace
  (hit.actions.mutate as (v: T) => void)(value);
}

// peek at a cached value without creating a resource. returns undefined
// when nothing is cached yet. used by streaming screens to decide whether
// to kick off a fresh scan or just render the previous one
export function peekCached<T>(key: string): T | undefined {
  const hit = store.get(key) as Entry<T> | undefined;
  if (!hit) return undefined;
  return hit.resource();
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
