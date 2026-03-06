import {
  createSignal,
  createMemo,
  For,
  onCleanup,
  onMount,
  Show,
  type JSX,
} from 'solid-js';
import { useNavigate, useSearchParams } from '@solidjs/router';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon } from '../components/Icon';
import { RadialSweep } from '../components/RadialSweep';
import { CountUp } from '../components/CountUp';
import {
  cancelScan,
  forgetScan,
  pauseScan,
  resumeScan,
  scanSnapshot,
  startScan,
  subscribeScan,
  type ScanEvent,
  type ScanProgress,
  type ScanState,
} from '../lib/scanner';
import { junkScan } from '../lib/junk';
import { formatBytes, formatCount, formatDuration, splitBytes } from '../lib/format';

// which scan we're fronting. ?kind= so Smart Scan's Rescan + System Junk's
// Rescan share the visual but hit different backends.
// - smart: streaming walker over $HOME, real events
// - junk: sync junk_scan, we play the anim for a min wall-clock window then
//   bounce back to /junk with fresh data
type ScanKind = 'smart' | 'junk';

// min window the anim plays when fronting a fast sync scan
const SYNC_SCAN_MIN_MS = 2400;

// smart scan in-progress screen. real data via scan://event, scan://progress,
// scan://done. startScan() on mount (no root = rust picks $HOME). subscribe
// wires three channels to signals. on unmount cancel+forget so the walker
// doesn't keep burning IO behind the user's back.
// log buffer capped at LOG_MAX, home dir scans hit thousands of events
// and an unbounded push balloons memory + stalls solid's reconciler.
const LOG_MAX = 200;

export default function SmartScanRunning(): JSX.Element {
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const kind: ScanKind = searchParams.kind === 'junk' ? 'junk' : 'smart';

  const [handleId, setHandleId] = createSignal<string | null>(null);
  const [roots, setRoots] = createSignal<string[]>([]);
  const [progress, setProgress] = createSignal<ScanProgress>(emptyProgress());
  const [log, setLog] = createSignal<ScanEvent[]>([]);
  const [error, setError] = createSignal<string | null>(null);

  let unsubscribe: (() => void) | null = null;
  let pollInterval: ReturnType<typeof setInterval> | null = null;
  let syncAnimTimer: ReturnType<typeof setInterval> | null = null;
  // set in onCleanup + stop() so in-flight sync scan promises don't stomp
  // signals or fire a late navigate after the user left
  let disposed = false;

  onMount(async () => {
    if (kind === 'junk') {
      await runSyncScan({
        label: 'System Junk',
        run: junkScan,
        onDone: () => navigate('/junk', { replace: true }),
      });
      return;
    }

    try {
      const handle = await startScan();
      setHandleId(handle.id);
      setRoots(handle.roots);

      unsubscribe = await subscribeScan(handle.id, {
        onEvent: (ev) => {
          // prepend, newest-first without reversing on every paint
          setLog((prev) => {
            const next = [ev, ...prev];
            return next.length > LOG_MAX ? next.slice(0, LOG_MAX) : next;
          });
        },
        onProgress: (p) => setProgress(p),
        onDone: (p) => {
          setProgress(p);
          // show final state for a beat, then pop back if not cancelled
          if (p.state === 'done') {
            window.setTimeout(() => {
              void forgetScan(handle.id);
              navigate('/scan', { replace: true });
            }, 1200);
          }
        },
      });

      // safety net for missed events (tab backgrounded when tauri fired),
      // poll every second
      pollInterval = setInterval(async () => {
        const id = handleId();
        if (!id) return;
        const snap = await scanSnapshot(id);
        if (snap) setProgress(snap);
      }, 1000);
    } catch (e) {
      setError(String(e));
    }
  });

  // front a sync scan w/ the streaming anim. fake curve in-ui, race it
  // against the real scan, nav only after min window AND scan returns so
  // the user always sees a visible scan regardless of backend speed
  async function runSyncScan<T extends { totalBytes?: number; totalItems?: number }>(args: {
    label: string;
    run: () => Promise<T>;
    onDone: (result: T) => void;
  }) {
    setRoots([args.label]);
    const startedAt = performance.now();

    // synthetic curve so bar + elapsed move. 120ms tick matches the real
    // streaming cadence closely enough
    syncAnimTimer = setInterval(() => {
      const elapsed = performance.now() - startedAt;
      setProgress((p) => ({ ...p, elapsedMs: elapsed, state: 'running' }));
    }, 120);

    try {
      const [result] = await Promise.all([
        args.run(),
        new Promise<void>((r) => setTimeout(r, SYNC_SCAN_MIN_MS)),
      ]);
      if (syncAnimTimer) {
        clearInterval(syncAnimTimer);
        syncAnimTimer = null;
      }
      if (disposed) return;
      // flash final totals for a beat so the user sees the scan produced
      // numbers before we leave
      setProgress({
        filesScanned: result.totalItems ?? 0,
        bytesScanned: result.totalBytes ?? 0,
        flaggedBytes: result.totalBytes ?? 0,
        flaggedItems: result.totalItems ?? 0,
        elapsedMs: performance.now() - startedAt,
        state: 'done',
        currentPath: null,
      });
      window.setTimeout(() => {
        if (disposed) return;
        args.onDone(result);
      }, 600);
    } catch (e) {
      if (syncAnimTimer) {
        clearInterval(syncAnimTimer);
        syncAnimTimer = null;
      }
      if (disposed) return;
      setError(String(e));
    }
  }

  onCleanup(() => {
    disposed = true;
    unsubscribe?.();
    if (pollInterval) clearInterval(pollInterval);
    if (syncAnimTimer) clearInterval(syncAnimTimer);
    const id = handleId();
    if (id) {
      // best-effort, completed scan just frees the slot
      void cancelScan(id).finally(() => void forgetScan(id));
    }
  });

  const state = createMemo<ScanState>(() => progress().state);
  const isPaused = () => state() === 'paused';
  const isDone = () => state() === 'done' || state() === 'cancelled';
  const isSync = () => kind !== 'smart';
  const returnPath = () => (kind === 'junk' ? '/junk' : '/scan');
  const sudsMood = () =>
    isPaused() ? 'sleepy' : state() === 'done' ? 'happy' : 'zoom';

  const toggle = async () => {
    // sync scans can't pause, tauri cmd runs to completion on the
    // blocking pool. button is disabled in that mode
    if (isSync()) return;
    const id = handleId();
    if (!id) return;
    if (isPaused()) {
      await resumeScan(id);
      // optimistic, next progress tick confirms
      setProgress((p) => ({ ...p, state: 'running' }));
    } else {
      await pauseScan(id);
      setProgress((p) => ({ ...p, state: 'paused' }));
    }
  };

  const stop = async () => {
    const id = handleId();
    if (id) {
      await cancelScan(id);
      await forgetScan(id);
    }
    // sync scans have no id to cancel, just nav back. in-flight junk_scan
    // finishes in the rust worker and its result is discarded, no side
    // effects
    navigate(returnPath(), { replace: true });
  };

  const currentPath = () => progress().currentPath ?? roots()[0] ?? '';

  // rough ETA, conservative. we don't know total tree size so we show
  // "estimating..." until enough signal
  const eta = createMemo(() => {
    const p = progress();
    if (p.elapsedMs < 500 || p.filesScanned < 50) return null;
    // heuristic: ~60% through after 5s. good enough, are later replaced by
    // with a per-root pre-count
    const fraction = Math.min(0.95, 0.1 + p.elapsedMs / 30_000);
    const totalMs = p.elapsedMs / fraction;
    return Math.max(0, totalMs - p.elapsedMs);
  });

  const progressPct = createMemo(() => {
    const p = progress();
    if (isDone()) return 100;
    // sync scans don't tick filesScanned until they resolve, drive the bar
    // off wall-clock vs the min display window
    if (isSync()) {
      return Math.min(95, Math.round((p.elapsedMs / SYNC_SCAN_MIN_MS) * 100));
    }
    if (p.filesScanned < 20) return 5;
    // pairs with the ETA heuristic above
    const fraction = Math.min(0.95, 0.1 + p.elapsedMs / 30_000);
    return Math.round(fraction * 100);
  });

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb={kind === 'junk' ? 'Cleanup · System Junk' : 'Overview · Smart Scan'}
        title="Scanning…"
        subtitle="Suds is rummaging. Feel free to keep working."
        right={
          <div style={{ display: 'flex', gap: '8px' }}>
            <button
              class="safai-btn safai-btn--ghost"
              onClick={toggle}
              disabled={isDone() || isSync()}
            >
              <Icon name={isPaused() ? 'play' : 'pause'} size={12} />{' '}
              {isPaused() ? 'Resume' : 'Pause scan'}
            </button>
            <button class="safai-btn safai-btn--ghost" onClick={stop}>
              <Icon name="x" size={12} /> Stop
            </button>
          </div>
        }
      />

      <div
        style={{
          flex: 1,
          display: 'flex',
          'flex-direction': 'column',
          'align-items': 'center',
          padding: '20px 40px 24px',
          gap: '16px',
          overflow: 'auto',
        }}
      >
        <Show when={error()}>
          <ErrorCard message={error()!} onDismiss={() => navigate(returnPath())} />
        </Show>

        <Show when={!error()}>
          {/* ring + suds at top. ring owns its footprint so siblings flow
              below, not underneath */}
          <RadialSweep progress={progressPct() / 100} size={240}>
            <Suds size={120} mood={sudsMood() as any} float />
          </RadialSweep>

          <HeroText
            currentPath={currentPath()}
            state={state()}
            isPaused={isPaused()}
          />

          <StatsRow progress={progress()} eta={eta()} />

          <ProgressBar pct={progressPct()} paused={isPaused()} />

          <LiveLog entries={log()} />
        </Show>
      </div>
    </div>
  );
}

// helpers

function emptyProgress(): ScanProgress {
  return {
    filesScanned: 0,
    bytesScanned: 0,
    flaggedBytes: 0,
    flaggedItems: 0,
    elapsedMs: 0,
    state: 'running',
    currentPath: null,
  };
}

// subcomponents

function HeroText(props: { currentPath: string; state: ScanState; isPaused: boolean }) {
  const headline = () => {
    switch (props.state) {
      case 'paused':
        return 'Paused. Say the word.';
      case 'cancelled':
        return 'Cancelled.';
      case 'done':
        return 'All done — packing up.';
      case 'running':
      default:
        return 'Rummaging through caches…';
    }
  };
  return (
    <div style={{ 'text-align': 'center' }}>
      <div
        style={{
          'font-size': '11px',
          color: 'var(--safai-cyan)',
          'letter-spacing': '0.18em',
          'text-transform': 'uppercase',
          'margin-bottom': '6px',
        }}
      >
        {props.isPaused ? 'Paused' : 'Scanning'} · Scanning
      </div>
      <h2 style={{ 'font-size': '28px', 'margin-bottom': '6px' }}>{headline()}</h2>
      <div
        class="mono"
        style={{
          'font-size': '13px',
          color: 'var(--safai-fg-2)',
          'font-family': 'var(--safai-font-mono)',
          'max-width': '80%',
          margin: '0 auto',
          overflow: 'hidden',
          'text-overflow': 'ellipsis',
          'white-space': 'nowrap',
        }}
        title={props.currentPath}
      >
        {props.currentPath || '—'}
      </div>
    </div>
  );
}

function StatsRow(props: { progress: ScanProgress; eta: number | null }) {
  const flagged = () => splitBytes(props.progress.flaggedBytes);
  const etaText = () =>
    props.eta == null ? '—' : formatDuration(props.eta);
  return (
    <div
      style={{
        display: 'grid',
        'grid-template-columns': 'repeat(4, 1fr)',
        gap: '12px',
        'margin-bottom': '20px',
        'max-width': '720px',
        margin: '0 auto 20px',
        width: '100%',
      }}
    >
      {/* animates so the number climbs, doesn't flash */}
      <StatCard label="Scanned" unit="files">
        <CountUp value={props.progress.filesScanned} format={(v) => formatCount(Math.round(v))} />
      </StatCard>
      {/* split into value/unit so we only animate the number */}
      <StatCard label="Found so far" unit={flagged().unit}>
        {flagged().value}
      </StatCard>
      <StatCard label="Elapsed" unit="">
        {formatDuration(props.progress.elapsedMs)}
      </StatCard>
      <StatCard label="ETA" unit="">
        {etaText()}
      </StatCard>
    </div>
  );
}

function StatCard(props: { label: string; unit: string; children: JSX.Element }) {
  return (
    <div
      class="safai-card"
      style={{
        padding: '14px',
        'text-align': 'center',
        background: 'oklch(0.20 0.01 240 / 0.7)',
        'backdrop-filter': 'blur(8px)',
      }}
    >
      <div
        style={{
          'font-size': '10px',
          color: 'var(--safai-fg-3)',
          'letter-spacing': '0.12em',
          'text-transform': 'uppercase',
          'margin-bottom': '4px',
        }}
      >
        {props.label}
      </div>
      <div
        class="num"
        style={{
          'font-size': '22px',
          'font-family': 'var(--safai-font-display)',
          'font-weight': 600,
          'letter-spacing': '-0.02em',
          'font-variant-numeric': 'tabular-nums',
        }}
      >
        {props.children}
        <Show when={props.unit}>
          <span
            style={{
              'font-size': '12px',
              color: 'var(--safai-fg-2)',
              'font-weight': 400,
              'margin-left': '4px',
            }}
          >
            {props.unit}
          </span>
        </Show>
      </div>
    </div>
  );
}

function ProgressBar(props: { pct: number; paused: boolean }) {
  return (
    <div
      role="progressbar"
      aria-valuenow={props.pct}
      aria-valuemin={0}
      aria-valuemax={100}
      style={{ 'max-width': '720px', margin: '0 auto 14px', width: '100%' }}
    >
      <div
        style={{
          height: '8px',
          background: 'var(--safai-bg-2)',
          'border-radius': '4px',
          overflow: 'hidden',
          position: 'relative',
          border: '1px solid var(--safai-line)',
        }}
      >
        <div
          style={{
            position: 'absolute',
            left: 0,
            top: 0,
            bottom: 0,
            width: `${props.pct}%`,
            background: props.paused
              ? 'linear-gradient(90deg, var(--safai-fg-3), var(--safai-fg-2))'
              : 'linear-gradient(90deg, var(--safai-cyan), var(--safai-lilac))',
            'border-radius': '4px',
            transition: 'width 0.3s ease-out, background 0.3s ease-out',
          }}
        />
      </div>
    </div>
  );
}

function LiveLog(props: { entries: ScanEvent[] }) {
  const rows = () => props.entries.slice(0, 8);
  return (
    <div
      style={{
        'max-width': '720px',
        margin: '0 auto',
        width: '100%',
        flex: 1,
        'min-height': 0,
      }}
    >
      <div
        style={{
          'font-size': '10px',
          color: 'var(--safai-fg-3)',
          'letter-spacing': '0.12em',
          'text-transform': 'uppercase',
          'margin-bottom': '8px',
        }}
      >
        Live findings
      </div>
      <div
        class="mono"
        style={{
          'font-size': '11px',
          'line-height': 1.8,
          color: 'var(--safai-fg-2)',
          background: 'oklch(0.14 0.008 240)',
          border: '1px solid var(--safai-line)',
          'border-radius': '10px',
          padding: '10px 14px',
          height: '170px',
          overflow: 'hidden',
        }}
      >
        <Show
          when={rows().length > 0}
          fallback={
            <div style={{ color: 'var(--safai-fg-3)', 'font-style': 'italic' }}>
              Warming up the sniffers…
            </div>
          }
        >
          <For each={rows()}>
            {(entry, i) => <LogRow entry={entry} fade={i() / (rows().length + 2)} />}
          </For>
        </Show>
      </div>
    </div>
  );
}

function LogRow(props: { entry: ScanEvent; fade: number }) {
  const color = () => {
    switch (props.entry.kind) {
      case 'found':
        return 'var(--safai-amber)';
      case 'safe':
        return 'var(--safai-mint)';
      default:
        return 'var(--safai-fg-3)';
    }
  };
  const badge = () => {
    switch (props.entry.kind) {
      case 'found':
        return '+ found';
      case 'safe':
        return '✓ safe';
      default:
        return '· scan';
    }
  };
  const ts = () => {
    const total = Math.floor(props.entry.elapsedMs / 1000);
    const m = Math.floor(total / 60);
    const s = total % 60;
    return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
  };
  const tail = () =>
    props.entry.kind === 'scan'
      ? props.entry.path
      : `${formatBytes(props.entry.bytes)} · ${props.entry.path}`;
  return (
    <div
      style={{
        display: 'flex',
        gap: '12px',
        opacity: 1 - props.fade * 0.5,
      }}
    >
      <span style={{ color: 'var(--safai-fg-3)' }}>{ts()}</span>
      <span
        style={{
          width: '52px',
          'font-size': '9px',
          'text-transform': 'uppercase',
          'letter-spacing': '0.08em',
          color: color(),
          'font-weight': 500,
          'flex-shrink': 0,
        }}
      >
        {badge()}
      </span>
      <span
        style={{
          color: props.entry.kind === 'scan' ? 'var(--safai-fg-2)' : 'var(--safai-fg-0)',
          flex: 1,
          'white-space': 'nowrap',
          overflow: 'hidden',
          'text-overflow': 'ellipsis',
        }}
        title={tail()}
      >
        {tail()}
      </span>
    </div>
  );
}

function ErrorCard(props: { message: string; onDismiss: () => void }) {
  return (
    <div
      class="safai-card"
      style={{
        padding: '24px 28px',
        display: 'flex',
        'align-items': 'center',
        gap: '20px',
        'margin-bottom': '20px',
        'max-width': '720px',
        width: '100%',
        border: '1px solid oklch(0.68 0.18 25 / 0.4)',
      }}
    >
      <Suds size={72} mood="shocked" />
      <div style={{ flex: 1 }}>
        <div style={{ 'font-size': '14px', color: 'var(--safai-fg-0)', 'margin-bottom': '4px' }}>
          Scan couldn't start
        </div>
        <div class="mono" style={{ 'font-size': '11px', color: 'var(--safai-fg-2)' }}>
          {props.message}
        </div>
      </div>
      <button class="safai-btn safai-btn--ghost" onClick={props.onDismiss}>
        <Icon name="x" size={12} /> Back
      </button>
    </div>
  );
}
