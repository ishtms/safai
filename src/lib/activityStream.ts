// shared activity stream. Memory + Activity screens both need
// activity://snapshot events; previously each screen called startActivity
// on mount and cancelled on unmount. tab-flip patterns leaked handles and
// doubled the backing thread during the overlap. this module owns ONE
// handle + ONE subscription for the app's lifetime and fans out to every
// subscriber.

import { createSignal } from 'solid-js';
import {
  activitySample,
  cancelActivity,
  forgetActivity,
  startActivity,
  startMockTicker,
  subscribeActivity,
  type ActivitySnapshot,
} from './activity';
import { isTauri } from './ipc';

const [latest, setLatest] = createSignal<ActivitySnapshot | null>(null);
const [history, setHistory] = createSignal<ActivitySnapshot[]>([]);

const HISTORY_CAP = 60;

let refcount = 0;
let handleId: string | null = null;
let unlisten: (() => void) | null = null;
let stopMock: (() => void) | null = null;
let boot: Promise<void> | null = null;

function push(snap: ActivitySnapshot) {
  setLatest(snap);
  setHistory((prev) => {
    const next = prev.concat(snap);
    return next.length > HISTORY_CAP ? next.slice(next.length - HISTORY_CAP) : next;
  });
}

async function bootOnce(): Promise<void> {
  if (boot) return boot;
  boot = (async () => {
    if (isTauri()) {
      try {
        const first = await activitySample();
        push(first);
      } catch {
        // subscription below will recover
      }
      try {
        unlisten = await subscribeActivity({ onSnapshot: push });
        const h = await startActivity({});
        handleId = h.id;
      } catch (e) {
        // eslint-disable-next-line no-console
        console.warn('startActivity failed', e);
      }
    } else {
      stopMock = startMockTicker(push);
    }
  })();
  return boot;
}

async function teardown(): Promise<void> {
  if (stopMock) {
    stopMock();
    stopMock = null;
  }
  if (unlisten) {
    unlisten();
    unlisten = null;
  }
  if (handleId) {
    const id = handleId;
    handleId = null;
    try {
      await cancelActivity(id);
    } catch {
      // best-effort
    }
    try {
      await forgetActivity(id);
    } catch {
      // best-effort
    }
  }
  boot = null;
}

// call from onMount, pair with releaseActivityStream() in onCleanup.
// first acquire boots the backing stream, subsequent acquires share it
export function acquireActivityStream(): void {
  refcount += 1;
  if (refcount === 1) {
    void bootOnce();
  }
}

// when the last subscriber releases we could tear the stream down, but
// activity polling is cheap and the rush of re-mounts from snappy nav
// costs more than keeping it hot. leave running, tear down only when
// the app window closes (which happens automatically)
export function releaseActivityStream(): void {
  refcount = Math.max(0, refcount - 1);
  // no tearing down on refcount==0 on purpose - keeps nav snappy
}

export function latestSnapshot() {
  return latest;
}

export function snapshotHistory() {
  return history;
}

// dev helper
export async function _teardownForTest(): Promise<void> {
  refcount = 0;
  await teardown();
  setLatest(null);
  setHistory([]);
}
