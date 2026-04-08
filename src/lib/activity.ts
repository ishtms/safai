// ts mirror of src-tauri/src/scanner/activity. wire format guarded by
// the rust module's *_camelcase tests, keep in sync.

import { invoke, listen } from './ipc';
import type { UnlistenFn } from '@tauri-apps/api/event';

export interface ProcessRow {
  pid: number;
  parentPid: number | null;
  name: string;
  command: string;
  user: string | null;
  /** sum of per-core %, like top / activity monitor */
  cpuPercent: number;
  memoryBytes: number;
  /** unix seconds, 0 if unknown */
  startTime: number;
  threads: number | null;
}

export interface MemorySnapshot {
  totalBytes: number;
  usedBytes: number;
  freeBytes: number;
  availableBytes: number;
  swapTotalBytes: number;
  swapUsedBytes: number;
  /** used/total * 100, clamped */
  pressurePercent: number;
}

export interface CpuSnapshot {
  coreCount: number;
  perCorePercent: number[];
  averagePercent: number;
}

export interface ActivitySnapshot {
  timestampMs: number;
  memory: MemorySnapshot;
  cpu: CpuSnapshot;
  processes: ProcessRow[];
  topByMemory: ProcessRow[];
  topByCpu: ProcessRow[];
  processCount: number;
  tick: number;
}

export interface ActivityHandle {
  id: string;
  intervalMs: number;
}

export const EVENT_ACTIVITY_SNAPSHOT = 'activity://snapshot';

// matches rust DEFAULT_INTERVAL_MS. 1s is the sweet spot: smooth
// sparklines, cheap cpu sampling, same as activity monitor / task mgr
export const DEFAULT_INTERVAL_MS = 1_000;
export const MIN_INTERVAL_MS = 200;
export const MAX_INTERVAL_MS = 60_000;
export const DEFAULT_TOP_N = 10;

export interface ActivityStartOptions {
  intervalMs?: number;
  topN?: number;
}

export function activitySample(topN: number = DEFAULT_TOP_N): Promise<ActivitySnapshot> {
  return invoke<ActivitySnapshot>('activity_sample', { topN }, () => mockSnapshot(0));
}

export function startActivity(opts: ActivityStartOptions = {}): Promise<ActivityHandle> {
  const { intervalMs, topN } = opts;
  return invoke<ActivityHandle>(
    'start_activity',
    { intervalMs, topN },
    () => ({ id: `mock-${Math.random().toString(36).slice(2, 10)}`, intervalMs: intervalMs ?? DEFAULT_INTERVAL_MS }),
  );
}

export function cancelActivity(handleId: string): Promise<boolean> {
  return invoke<boolean>('cancel_activity', { handleId }, () => false);
}

export function forgetActivity(handleId: string): Promise<boolean> {
  return invoke<boolean>('forget_activity', { handleId }, () => false);
}

export function setActivityInterval(handleId: string, intervalMs: number): Promise<boolean> {
  return invoke<boolean>('set_activity_interval', { handleId, intervalMs }, () => false);
}

export function refreshActivity(handleId: string): Promise<boolean> {
  return invoke<boolean>('refresh_activity', { handleId }, () => false);
}

export function killProcess(pid: number, force: boolean = false): Promise<void> {
  return invoke<void>('kill_process', { pid, force }, () => undefined);
}

export interface ActivitySubscriptions {
  onSnapshot?: (snap: ActivitySnapshot) => void;
}

export async function subscribeActivity(subs: ActivitySubscriptions): Promise<UnlistenFn> {
  const unlisteners: UnlistenFn[] = [];
  if (subs.onSnapshot) {
    unlisteners.push(
      await listen<ActivitySnapshot>(EVENT_ACTIVITY_SNAPSHOT, (s) => subs.onSnapshot!(s)),
    );
  }
  return () => {
    for (const u of unlisteners) {
      try {
        u();
      } catch {
        // best-effort
      }
    }
  };
}

// mock tick stream for browser-only dev
export function startMockTicker(
  cb: (snap: ActivitySnapshot) => void,
  intervalMs: number = DEFAULT_INTERVAL_MS,
): () => void {
  let tick = 0;
  const id = window.setInterval(() => {
    cb(mockSnapshot(tick++));
  }, intervalMs);
  // fire one now so ui isn't blank for a full tick
  cb(mockSnapshot(tick++));
  return () => window.clearInterval(id);
}

// selectors + helpers

export type SortKey = 'memory' | 'cpu' | 'name' | 'pid';
export type SortDirection = 'asc' | 'desc';

export function sortProcesses(
  rows: ProcessRow[],
  key: SortKey,
  dir: SortDirection = 'desc',
): ProcessRow[] {
  const out = rows.slice();
  out.sort((a, b) => {
    let cmp = 0;
    switch (key) {
      case 'memory':
        cmp = b.memoryBytes - a.memoryBytes;
        break;
      case 'cpu':
        // NaN goes to the end regardless of dir
        if (Number.isNaN(a.cpuPercent) && !Number.isNaN(b.cpuPercent)) return 1;
        if (!Number.isNaN(a.cpuPercent) && Number.isNaN(b.cpuPercent)) return -1;
        cmp = b.cpuPercent - a.cpuPercent;
        break;
      case 'name':
        cmp = a.name.localeCompare(b.name);
        break;
      case 'pid':
        cmp = a.pid - b.pid;
        break;
    }
    if (cmp === 0) cmp = a.pid - b.pid;
    return dir === 'asc' ? -cmp : cmp;
  });
  return out;
}

export function filterProcesses(rows: ProcessRow[], query: string): ProcessRow[] {
  const q = query.trim().toLowerCase();
  if (!q) return rows;
  return rows.filter((r) =>
    r.name.toLowerCase().includes(q)
    || r.command.toLowerCase().includes(q)
    || String(r.pid) === q,
  );
}

/** color bucket for the pressure pill / gauge */
export function pressureTone(pct: number): 'ok' | 'warn' | 'alert' {
  if (pct >= 85) return 'alert';
  if (pct >= 65) return 'warn';
  return 'ok';
}

export function pressureColour(pct: number): string {
  switch (pressureTone(pct)) {
    case 'alert':
      return 'var(--safai-coral)';
    case 'warn':
      return 'var(--safai-amber, var(--safai-fg-1))';
    case 'ok':
      return 'var(--safai-cyan)';
  }
}

// mocks for plain-browser dev

function mockSnapshot(tick: number): ActivitySnapshot {
  const nowMs = Date.now();
  const total = 16 * 1024 * 1024 * 1024;
  const base = 8 * 1024 * 1024 * 1024;
  // wiggle used mem per tick so sparkline isn't flat
  const wiggle = Math.sin(tick / 5) * 0.08 + 1;
  const used = Math.round(base * wiggle);
  const processes: ProcessRow[] = [
    mockProc(1, 'WindowServer', 1_100_000_000, 15 + Math.random() * 5),
    mockProc(2, 'Safari', 900_000_000, 22 + Math.random() * 10),
    mockProc(3, 'Xcode', 1_400_000_000, 18 + Math.random() * 8),
    mockProc(4, 'safai', 220_000_000, 1 + Math.random() * 2),
    mockProc(5, 'zsh', 18_000_000, 0.1),
    mockProc(6, 'Docker', 780_000_000, 4 + Math.random() * 3),
    mockProc(7, 'node', 620_000_000, 6 + Math.random() * 4),
    mockProc(8, 'kernel_task', 1_900_000_000, 8 + Math.random() * 4),
    mockProc(9, 'launchd', 14_000_000, 0),
    mockProc(10, 'Slack', 640_000_000, 5 + Math.random() * 3),
  ];
  const topByMemory = processes.slice().sort((a, b) => b.memoryBytes - a.memoryBytes).slice(0, 10);
  const topByCpu = processes.slice().sort((a, b) => b.cpuPercent - a.cpuPercent).slice(0, 10);
  return {
    timestampMs: nowMs,
    memory: {
      totalBytes: total,
      usedBytes: used,
      freeBytes: total - used,
      availableBytes: total - used,
      swapTotalBytes: 2 * 1024 * 1024 * 1024,
      swapUsedBytes: 100 * 1024 * 1024,
      pressurePercent: (used / total) * 100,
    },
    cpu: {
      coreCount: 8,
      perCorePercent: Array.from({ length: 8 }, () => Math.random() * 60),
      averagePercent: 20 + Math.random() * 20,
    },
    processes,
    topByMemory,
    topByCpu,
    processCount: processes.length,
    tick,
  };
}

function mockProc(pid: number, name: string, mem: number, cpu: number): ProcessRow {
  return {
    pid,
    parentPid: 1,
    name,
    command: `/Applications/${name}.app/Contents/MacOS/${name}`,
    user: 'ish',
    cpuPercent: cpu,
    memoryBytes: mem,
    startTime: Math.floor(Date.now() / 1000) - 3600,
    threads: 12,
  };
}
