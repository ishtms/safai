import type { OS } from '../components/OSChip';

// detect host os via the tauri os plugin, UA fallback for plain vite dev
export async function detectOS(): Promise<OS> {
  try {
    const { platform } = await import('@tauri-apps/plugin-os');
    const p = platform();
    if (p === 'macos' || p === 'ios') return 'mac';
    if (p === 'windows') return 'win';
    return 'linux';
  } catch {
    const ua = typeof navigator !== 'undefined' ? navigator.userAgent : '';
    if (/Mac|iPhone|iPad/i.test(ua)) return 'mac';
    if (/Win/i.test(ua)) return 'win';
    return 'linux';
  }
}
