// thin tauri ipc wrapper. one place to stub commands under plain
// `pnpm dev` so screens work without the rust runtime attached

import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen as tauriListen, type UnlistenFn } from '@tauri-apps/api/event';

function isTauri(): boolean {
  // __TAURI_INTERNALS__ is injected by the tauri shell. undefined in a
  // plain browser tab, invoke would hang forever otherwise
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export { isTauri };

// fallback returns when not running under tauri, so browser dev reloads
// don't spin forever
export async function invoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
  fallback?: () => T | Promise<T>,
): Promise<T> {
  if (!isTauri()) {
    if (fallback) return fallback();
    throw new Error(`Tauri runtime not available (cmd: ${cmd})`);
  }
  return tauriInvoke<T>(cmd, args);
}

// subscribe to a tauri event. outside tauri resolves to a no-op
// unlisten so callers can await listen(...) without branching.
// scanner uses this for scan://event, scan://progress, scan://done
export async function listen<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) {
    return () => {};
  }
  return tauriListen<T>(event, (evt) => handler(evt.payload as T));
}
