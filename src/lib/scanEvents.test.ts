import { beforeEach, describe, expect, it, vi } from 'vitest';
import { subscribeActivity, type ActivitySnapshot } from './activity';
import { subscribeDuplicates, type DuplicateReport } from './duplicates';
import { subscribeLargeOld, type LargeOldReport } from './largeold';
import { subscribeMalware, type MalwareReport } from './malware';
import { subscribeScan, type ScanProgress } from './scanner';
import { subscribeTreemap, type TreemapResponse } from './treemap';

const listeners = vi.hoisted(() => new Map<string, Array<(payload: unknown) => void>>());

vi.mock('./ipc', () => ({
  invoke: vi.fn(),
  listen: vi.fn(async (event: string, handler: (payload: unknown) => void) => {
    const bucket = listeners.get(event) ?? [];
    bucket.push(handler);
    listeners.set(event, bucket);
    return () => {
      listeners.set(
        event,
        (listeners.get(event) ?? []).filter((h) => h !== handler),
      );
    };
  }),
}));

function emit(event: string, payload: unknown) {
  for (const handler of listeners.get(event) ?? []) handler(payload);
}

function envelope<T>(
  handleId: string,
  payload: T,
  sequence: number = 1,
  kind: string = 'progress',
  terminal: boolean = false,
) {
  return {
    kind,
    handleId,
    phase: null,
    payload,
    sequence,
    terminal,
  };
}

describe('handled scan events', () => {
  beforeEach(() => {
    listeners.clear();
  });

  it('filters smart scan progress and done by handle', async () => {
    const progress = vi.fn();
    const done = vi.fn();
    await subscribeScan('scan-a', { onProgress: progress, onDone: done });
    const payload: ScanProgress = {
      filesScanned: 1,
      bytesScanned: 2,
      flaggedBytes: 0,
      flaggedItems: 0,
      elapsedMs: 3,
      state: 'running',
      currentPath: null,
      volumeUsedBytes: 0,
      volumeTotalBytes: 0,
    };

    emit('scan://progress', envelope('scan-b', payload));
    emit('scan://done', envelope('scan-a', payload, 1, 'done', true));

    expect(progress).not.toHaveBeenCalled();
    expect(done).toHaveBeenCalledWith(payload);
  });

  it('filters treemap events by handle', async () => {
    const done = vi.fn();
    await subscribeTreemap('tree-a', { onDone: done });
    const payload: TreemapResponse = {
      root: '~',
      totalBytes: 1,
      totalFiles: 1,
      tiles: [],
      biggest: [],
      scannedAt: 1,
      durationMs: 1,
    };

    emit('treemap://done', envelope('tree-b', payload, 1, 'done', true));
    emit('treemap://done', envelope('tree-a', payload, 1, 'done', true));

    expect(done).toHaveBeenCalledTimes(1);
    expect(done).toHaveBeenCalledWith(payload);
  });

  it('filters duplicates events by handle', async () => {
    const progress = vi.fn();
    await subscribeDuplicates('dupes-a', { onProgress: progress });
    const payload: DuplicateReport = {
      root: '~',
      groups: [],
      totalFilesScanned: 10,
      totalGroups: 0,
      wastedBytes: 0,
      durationMs: 1,
      phase: 'walking',
      candidatesRemaining: 10,
    };

    emit('dupes://progress', envelope('dupes-b', payload));
    emit('dupes://progress', envelope('dupes-a', payload));

    expect(progress).toHaveBeenCalledTimes(1);
    expect(progress).toHaveBeenCalledWith(payload);
  });

  it('filters large-old events by handle', async () => {
    const done = vi.fn();
    await subscribeLargeOld('large-a', { onDone: done });
    const payload: LargeOldReport = {
      root: '~',
      files: [],
      totalMatched: 0,
      totalBytes: 0,
      totalFilesScanned: 10,
      durationMs: 1,
      phase: 'done',
      minBytes: 1,
      minDaysIdle: 1,
    };

    emit('large-old://done', envelope('large-b', payload, 1, 'done', true));
    emit('large-old://done', envelope('large-a', payload, 1, 'done', true));

    expect(done).toHaveBeenCalledTimes(1);
    expect(done).toHaveBeenCalledWith(payload);
  });

  it('filters malware events by handle', async () => {
    const progress = vi.fn();
    const done = vi.fn();
    await subscribeMalware('mw-a', { onProgress: progress, onDone: done });
    const payload: MalwareReport = {
      findings: [],
      totalFindingCount: 0,
      displayedFindingCount: 0,
      findingsTruncated: false,
      signatureCatalogVersion: 'builtin-eicar-v1',
      signatureCount: 1,
      criticalCount: 0,
      mediumCount: 0,
      infoCount: 0,
      totalFilesScanned: 10,
      scannedAt: 1,
      durationMs: 1,
      platform: 'linux',
      phase: 'done',
      hasSignatureHit: false,
    };

    emit('malware://progress', envelope('mw-b', payload));
    emit('malware://done', envelope('mw-a', payload, 1, 'done', true));

    expect(progress).not.toHaveBeenCalled();
    expect(done).toHaveBeenCalledWith(payload);
  });

  it('filters activity snapshots by handle', async () => {
    const snapshot = vi.fn();
    await subscribeActivity('act-a', { onSnapshot: snapshot });
    const payload: ActivitySnapshot = {
      timestampMs: 1,
      memory: {
        totalBytes: 1,
        usedBytes: 1,
        freeBytes: 0,
        availableBytes: 0,
        swapTotalBytes: 0,
        swapUsedBytes: 0,
        pressurePercent: 100,
      },
      cpu: { coreCount: 1, perCorePercent: [1], averagePercent: 1 },
      processes: [],
      topByMemory: [],
      topByCpu: [],
      processCount: 0,
      tick: 1,
    };

    emit('activity://snapshot', envelope('act-b', payload, 1, 'snapshot'));
    emit('activity://snapshot', envelope('act-a', payload, 1, 'snapshot'));

    expect(snapshot).toHaveBeenCalledTimes(1);
    expect(snapshot).toHaveBeenCalledWith(payload);
  });

  it('ignores stale events for the same handle', async () => {
    const progress = vi.fn();
    await subscribeDuplicates('dupes-a', { onProgress: progress });
    const newer: DuplicateReport = {
      root: '~',
      groups: [],
      totalFilesScanned: 20,
      totalGroups: 0,
      wastedBytes: 0,
      durationMs: 2,
      phase: 'walking',
      candidatesRemaining: 20,
    };
    const older = { ...newer, totalFilesScanned: 10, candidatesRemaining: 10 };

    emit('dupes://progress', envelope('dupes-a', newer, 2));
    emit('dupes://progress', envelope('dupes-a', older, 1));

    expect(progress).toHaveBeenCalledTimes(1);
    expect(progress).toHaveBeenCalledWith(newer);
  });
});
