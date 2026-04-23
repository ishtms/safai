// thin wrapper over @tauri-apps/plugin-updater so call sites stay
// framework-free and the ssr/browser build doesn't crash on import of
// a tauri-only module.
// everything no-ops in a plain browser. updater is tauri-only, better
// to render a disabled banner than crash with "__TAURI__ is not defined"

import { isTauri } from './ipc';

export type UpdateStatus =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'upToDate' }
  | { kind: 'available'; version: string; notes: string | null }
  | { kind: 'downloading'; downloaded: number; total: number | null }
  | { kind: 'ready' }
  | { kind: 'error'; message: string };

// check for update. returns manifest when available, null when current
// or updater is disabled. errors bubble so caller can render a banner
export async function checkForUpdate(): Promise<
  | null
  | { version: string; notes: string | null; apply: () => Promise<void> }
> {
  if (!isTauri()) return null;
  const { check } = await import('@tauri-apps/plugin-updater');
  const update = await check();
  if (!update) return null;

  return {
    version: update.version,
    notes: update.body ?? null,
    apply: async () => {
      await update.downloadAndInstall();
      const { relaunch } = await import('@tauri-apps/plugin-process');
      await relaunch();
    },
  };
}

// like checkForUpdate but streams download progress via the callback.
// separate fn because most callers just want yes/no on boot, this one
// runs once user opts in to install
export async function downloadWithProgress(
  onProgress: (downloaded: number, total: number | null) => void,
): Promise<void> {
  if (!isTauri()) return;
  const { check } = await import('@tauri-apps/plugin-updater');
  const update = await check();
  if (!update) return;
  let downloaded = 0;
  let total: number | null = null;
  await update.downloadAndInstall((ev) => {
    switch (ev.event) {
      case 'Started':
        total = ev.data.contentLength ?? null;
        onProgress(0, total);
        break;
      case 'Progress':
        downloaded += ev.data.chunkLength;
        onProgress(downloaded, total);
        break;
      case 'Finished':
        onProgress(total ?? downloaded, total);
        break;
    }
  });
  const { relaunch } = await import('@tauri-apps/plugin-process');
  await relaunch();
}
