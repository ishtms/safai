import {
  createMemo,
  createResource,
  createSignal,
  For,
  onCleanup,
  Show,
} from 'solid-js';
import { invalidate, KEY_LARGE_OLD, peekCached, setCached } from '../lib/scanCache';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon } from '../components/Icon';
import { ConfirmDeleteModal } from '../components/ConfirmDeleteModal';
import {
  bucketColour,
  bucketFor,
  bucketLabel,
  cancelLargeOld,
  forgetLargeOld,
  phaseLabel,
  revealInFileManager,
  startLargeOld,
  subscribeLargeOld,
  type Bucket,
  type FileSummary,
  type LargeOldReport,
} from '../lib/largeold';
import {
  commitDelete,
  graveyardStats,
  previewDelete,
  restoreLast,
  type DeletePlan,
  type DeleteResult,
  type GraveyardStats,
} from '../lib/cleaner';
import {
  formatBytes,
  formatCount,
  splitBytes,
  truncateMiddle,
} from '../lib/format';

// large & old. scatter plot w/ log-size Y + log-idle-days X so you spot
// clusters ("everything in Downloads from late 2023") without scanning rows.
// table mirrors selection + sorts. selection is a Set<path>, clean routes
// through the shared cleaner like Junk/Duplicates.

const TABLE_PAGE_STEP = 200;

type SortField = 'bytes' | 'idleDays' | 'path' | 'extension';
type SortDir = 'asc' | 'desc';

export default function LargeOld() {
  // seed from cache so tab-switching doesn't re-kick the walk. first
  // visit returns undefined and we start a scan; later visits hydrate
  // instantly from the last completed report
  const cachedInitial = peekCached<LargeOldReport>(KEY_LARGE_OLD) ?? null;
  const [response, setResponseRaw] = createSignal<LargeOldReport | null>(cachedInitial);
  // wrap setResponse so the module cache stays in sync with on-screen state
  const setResponse = (next: LargeOldReport | null) => {
    setResponseRaw(next);
    if (next) setCached(KEY_LARGE_OLD, next);
  };
  const [scanning, setScanning] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [hovered, setHovered] = createSignal<string | null>(null);
  const [visibleRows, setVisibleRows] = createSignal(TABLE_PAGE_STEP);
  const [sortField, setSortField] = createSignal<SortField>('bytes');
  const [sortDir, setSortDir] = createSignal<SortDir>('desc');
  const [bucketFilter, setBucketFilter] = createSignal<Bucket | 'all'>('all');

  let currentHandle: string | null = null;
  let unsubscribe: (() => void) | null = null;

  const stopWalk = async () => {
    if (currentHandle) {
      try {
        await cancelLargeOld(currentHandle);
        await forgetLargeOld(currentHandle);
      } catch {
        // best-effort
      }
      currentHandle = null;
    }
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
  };

  const startWalk = async () => {
    await stopWalk();
    setError(null);
    setScanning(true);
    invalidate(KEY_LARGE_OLD);
    setResponseRaw(null);
    setSelected(new Set<string>());
    setVisibleRows(TABLE_PAGE_STEP);
    try {
      unsubscribe = await subscribeLargeOld({
        onProgress: (r) => setResponse(r),
        onDone: (r) => {
          setResponse(r);
          setScanning(false);
        },
      });
      const handle = await startLargeOld({});
      currentHandle = handle.id;
    } catch (e) {
      setError(String(e));
      setScanning(false);
    }
  };

  // only walk on first visit. later mounts render the cached report
  if (!cachedInitial) {
    void startWalk();
  }
  onCleanup(() => {
    void stopWalk();
  });

  const allFiles = () => response()?.files ?? [];
  const filteredFiles = createMemo<FileSummary[]>(() => {
    const files = allFiles();
    const b = bucketFilter();
    if (b === 'all') return files;
    return files.filter((f) => bucketFor(f.extension) === b);
  });

  const sortedFiles = createMemo<FileSummary[]>(() => {
    const files = filteredFiles().slice();
    const field = sortField();
    const dir = sortDir();
    const mult = dir === 'asc' ? 1 : -1;
    files.sort((a, b) => {
      let cmp = 0;
      switch (field) {
        case 'bytes':
          cmp = a.bytes - b.bytes;
          break;
        case 'idleDays':
          cmp = a.idleDays - b.idleDays;
          break;
        case 'path':
          cmp = a.path.localeCompare(b.path);
          break;
        case 'extension':
          cmp = a.extension.localeCompare(b.extension);
          break;
      }
      return cmp * mult || a.path.localeCompare(b.path);
    });
    return files;
  });

  const toggleSort = (field: SortField) => {
    if (sortField() === field) {
      setSortDir((d) => (d === 'asc' ? 'desc' : 'asc'));
    } else {
      setSortField(field);
      setSortDir(field === 'path' || field === 'extension' ? 'asc' : 'desc');
    }
  };

  const toggleSelected = (path: string) => {
    const next = new Set(selected());
    if (next.has(path)) next.delete(path);
    else next.add(path);
    setSelected(next);
  };
  const selectAllVisible = () => {
    const next = new Set(selected());
    for (const f of filteredFiles()) next.add(f.path);
    setSelected(next);
  };
  const clearSelection = () => setSelected(new Set<string>());

  const selectedBytes = createMemo(() => {
    const s = selected();
    if (s.size === 0) return 0;
    let total = 0;
    for (const f of allFiles()) if (s.has(f.path)) total += f.bytes;
    return total;
  });

  const [pendingPlan, setPendingPlan] = createSignal<DeletePlan | null>(null);
  const [planError, setPlanError] = createSignal<string | null>(null);
  const [committing, setCommitting] = createSignal(false);
  const [commitResult, setCommitResult] = createSignal<DeleteResult | null>(null);
  const [restoring, setRestoring] = createSignal(false);
  const [restoreMessage, setRestoreMessage] = createSignal<string | null>(null);
  const [stats, { refetch: refetchStats }] =
    createResource<GraveyardStats>(graveyardStats);

  const openConfirm = async () => {
    setPlanError(null);
    setCommitResult(null);
    const paths = Array.from(selected());
    if (paths.length === 0) return;
    try {
      const plan = await previewDelete(paths);
      setPendingPlan(plan);
    } catch (e) {
      setPlanError(String(e));
    }
  };

  const confirmClean = async () => {
    const plan = pendingPlan();
    if (!plan || committing()) return;
    setCommitting(true);
    try {
      const result = await commitDelete(plan.token);
      setCommitResult(result);
      setPendingPlan(null);
      // drop deleted paths from selection + rescan so the list reflects
      // what's actually still on disk
      const committed = new Set(result.committed);
      setSelected((s) => {
        const next = new Set<string>();
        for (const p of s) if (!committed.has(p)) next.add(p);
        return next;
      });
      void startWalk();
      refetchStats();
    } catch (e) {
      setPlanError(String(e));
    } finally {
      setCommitting(false);
    }
  };

  const doRestore = async () => {
    if (restoring()) return;
    setRestoring(true);
    setRestoreMessage(null);
    try {
      const r = await restoreLast();
      const n = r.restored.length;
      setRestoreMessage(
        n === 0
          ? 'Nothing to restore.'
          : `Restored ${formatCount(n)} item${n === 1 ? '' : 's'} (${formatBytes(
              r.bytesRestored,
            )}).`,
      );
      void startWalk();
      refetchStats();
    } catch (e) {
      setRestoreMessage(`Couldn't restore: ${String(e)}`);
    } finally {
      setRestoring(false);
    }
  };

  const doReveal = async (path: string) => {
    try {
      await revealInFileManager(path);
    } catch (e) {
      setPlanError(`Couldn't reveal: ${String(e)}`);
    }
  };

  const visibleRowsSlice = createMemo<FileSummary[]>(() => {
    const files = sortedFiles();
    const n = visibleRows();
    return n >= files.length ? files : files.slice(0, n);
  });

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Cleanup"
        title="Large & Old"
        subtitle="Big files you haven't touched in a long time. Safe to archive or delete."
        right={
          <div style={{ display: 'flex', gap: '8px', 'align-items': 'center' }}>
            <span style={{ 'font-size': '12px', color: 'var(--safai-fg-2)' }}>
              Selected:{' '}
              <span class="num" style={{ color: 'var(--safai-fg-0)', 'font-weight': 500 }}>
                {formatBytes(selectedBytes())}
              </span>
            </span>
            <button
              class="safai-btn safai-btn--ghost"
              onClick={() => void startWalk()}
              disabled={scanning()}
              aria-busy={scanning()}
            >
              <span class={scanning() ? 'safai-spin' : ''} style={{ display: 'inline-flex' }}>
                <Icon name="refresh" size={12} />
              </span>{' '}
              {scanning() ? 'Scanning…' : 'Rescan'}
            </button>
            <button
              class="safai-btn safai-btn--ghost"
              onClick={doRestore}
              disabled={restoring() || scanning()}
              title="Restore the most recent clean"
            >
              <span class={restoring() ? 'safai-spin' : ''} style={{ display: 'inline-flex' }}>
                <Icon name="refresh" size={12} />
              </span>{' '}
              {restoring() ? 'Restoring…' : 'Restore last'}
            </button>
            <button
              class="safai-btn safai-btn--primary"
              onClick={openConfirm}
              disabled={scanning() || committing() || selected().size === 0}
            >
              <Icon name="trash" size={12} color="oklch(0.18 0.02 240)" /> Clean selected
            </button>
          </div>
        }
      />

      <div
        style={{
          flex: 1,
          overflow: 'auto',
          padding: '24px 28px 40px',
          display: 'flex',
          'flex-direction': 'column',
          gap: '18px',
        }}
      >
        <HeroStrip response={response()} scanning={scanning()} />

        <Show when={error()}>
          <ErrorCard message={error()!} onRetry={() => void startWalk()} />
        </Show>

        <Show when={!error() && response() && allFiles().length > 0}>
          <ScatterPanel
            files={filteredFiles()}
            selected={selected()}
            hovered={hovered()}
            onHover={setHovered}
            onToggle={toggleSelected}
          />
          <BucketLegend
            files={allFiles()}
            filter={bucketFilter()}
            onSelect={(b) => setBucketFilter(b)}
          />
          <TableToolbar
            total={filteredFiles().length}
            selectedCount={selected().size}
            onSelectAll={selectAllVisible}
            onClear={clearSelection}
          />
          <FilesTable
            files={visibleRowsSlice()}
            selected={selected()}
            sortField={sortField()}
            sortDir={sortDir()}
            onSort={toggleSort}
            onToggle={toggleSelected}
            onReveal={(p) => void doReveal(p)}
            onHover={setHovered}
            hovered={hovered()}
          />
          <Show when={sortedFiles().length > visibleRows()}>
            <button
              class="safai-card safai-card--hover"
              onClick={() =>
                setVisibleRows((n) =>
                  Math.min(n + TABLE_PAGE_STEP, sortedFiles().length),
                )
              }
              style={{
                padding: '14px 18px',
                display: 'flex',
                'align-items': 'center',
                gap: '12px',
                cursor: 'pointer',
                background: 'var(--safai-bg-2)',
                border: '1px dashed var(--safai-line)',
                'text-align': 'left',
                'flex-shrink': 0,
              }}
            >
              <Icon name="chevronD" size={12} color="var(--safai-cyan)" />
              <span style={{ 'font-size': '13px', color: 'var(--safai-fg-0)' }}>
                Show next{' '}
                <span class="num">
                  {Math.min(TABLE_PAGE_STEP, sortedFiles().length - visibleRows())}
                </span>{' '}
                of{' '}
                <span class="num">
                  {formatCount(sortedFiles().length - visibleRows())}
                </span>{' '}
                hidden rows
              </span>
            </button>
          </Show>
        </Show>

        <Show when={!error() && response() && allFiles().length === 0 && !scanning()}>
          <EmptyState />
        </Show>

        <Show when={scanning() && allFiles().length === 0}>
          <ScanningCard response={response()} />
        </Show>

        <Show when={response()}>
          {(r) => (
            <div
              style={{
                'margin-top': '6px',
                'font-size': '11px',
                color: 'var(--safai-fg-3)',
                display: 'flex',
                gap: '12px',
                'align-items': 'center',
                'flex-wrap': 'wrap',
              }}
              aria-live="polite"
            >
              <span>
                Scanned{' '}
                <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
                  {formatCount(r().totalFilesScanned)}
                </span>{' '}
                files
              </span>
              <span>·</span>
              <span>
                <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
                  {formatCount(r().totalMatched)}
                </span>{' '}
                matched
              </span>
              <span>·</span>
              <span>
                ≥{' '}
                <span class="num">{formatBytes(r().minBytes)}</span>
                {' · '}idle{' '}
                <span class="num">{formatCount(r().minDaysIdle)}</span>{' '}
                days
              </span>
              <span>·</span>
              <span>{Math.max(1, Math.round(r().durationMs))} ms</span>
              <Show when={(stats()?.batchCount ?? 0) > 0}>
                <span>·</span>
                <span>
                  Graveyard:{' '}
                  <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
                    {formatBytes(stats()?.totalBytes ?? 0)}
                  </span>
                </span>
              </Show>
            </div>
          )}
        </Show>
      </div>

      <Show when={planError() || commitResult() || restoreMessage()}>
        <StatusBanner
          error={planError()}
          commit={commitResult()}
          restore={restoreMessage()}
          onDismiss={() => {
            setPlanError(null);
            setCommitResult(null);
            setRestoreMessage(null);
          }}
        />
      </Show>

      <Show when={pendingPlan()}>
        {(plan) => (
          <ConfirmDeleteModal
            plan={plan()}
            committing={committing()}
            onCancel={() => {
              if (!committing()) setPendingPlan(null);
            }}
            onConfirm={confirmClean}
          />
        )}
      </Show>
    </div>
  );
}

// hero

function HeroStrip(props: { response: LargeOldReport | null; scanning: boolean }) {
  const bytes = () => props.response?.totalBytes ?? 0;
  const matched = () => props.response?.totalMatched ?? 0;
  const split = () => splitBytes(bytes());
  return (
    <div
      class="safai-card safai-sheen"
      style={{
        padding: '20px 24px',
        display: 'flex',
        'align-items': 'center',
        gap: '20px',
        background: 'linear-gradient(135deg, oklch(0.22 0.02 240), oklch(0.20 0.02 260))',
        border: '1px solid oklch(0.82 0.14 200 / 0.25)',
      }}
    >
      <Suds
        size={64}
        mood={props.scanning ? 'happy' : matched() > 0 ? 'shocked' : 'happy'}
        float={props.scanning}
      />
      <div style={{ flex: 1, 'min-width': 0 }}>
        <div
          style={{
            'font-size': '10px',
            color: 'var(--safai-fg-3)',
            'letter-spacing': '0.12em',
            'text-transform': 'uppercase',
            'margin-bottom': '4px',
          }}
        >
          Reclaimable
        </div>
        <div style={{ display: 'flex', 'align-items': 'baseline', gap: '10px' }}>
          <div
            class="num"
            style={{
              'font-size': '36px',
              'font-weight': 600,
              'font-family': 'var(--safai-font-display)',
              'letter-spacing': '-0.04em',
              'line-height': 1,
              color: 'var(--safai-cyan)',
              'font-variant-numeric': 'tabular-nums',
            }}
          >
            {split().value}
          </div>
          <div style={{ 'font-size': '16px', color: 'var(--safai-fg-1)', 'font-weight': 500 }}>
            {split().unit}
          </div>
          <div style={{ 'font-size': '12px', color: 'var(--safai-fg-3)' }}>
            across {formatCount(matched())} file{matched() === 1 ? '' : 's'}
          </div>
          <Show when={props.scanning}>
            <ScanningPill />
          </Show>
        </div>
      </div>
    </div>
  );
}

function ScanningPill() {
  return (
    <div
      style={{
        'margin-left': '8px',
        display: 'inline-flex',
        'align-items': 'center',
        gap: '6px',
        padding: '2px 10px',
        'border-radius': '999px',
        background: 'oklch(0.82 0.14 200 / 0.15)',
        border: '1px solid oklch(0.82 0.14 200 / 0.35)',
        color: 'var(--safai-cyan)',
        'font-size': '10px',
        'letter-spacing': '0.08em',
        'text-transform': 'uppercase',
      }}
    >
      <span
        style={{
          width: '6px',
          height: '6px',
          'border-radius': '50%',
          background: 'var(--safai-cyan)',
          animation: 'safai-shimmer 1.2s ease-in-out infinite',
        }}
      />
      Walking
    </div>
  );
}

// scatter plot

interface ScatterProps {
  files: FileSummary[];
  selected: Set<string>;
  hovered: string | null;
  onHover: (path: string | null) => void;
  onToggle: (path: string) => void;
}

function ScatterPanel(props: ScatterProps) {
  const bounds = createMemo(() => {
    const files = props.files;
    if (files.length === 0) {
      return { minAge: 1, maxAge: 365, minBytes: 1, maxBytes: 1 };
    }
    let minAge = Infinity,
      maxAge = 0,
      minBytes = Infinity,
      maxBytes = 0;
    for (const f of files) {
      if (f.idleDays < minAge) minAge = f.idleDays;
      if (f.idleDays > maxAge) maxAge = f.idleDays;
      if (f.bytes < minBytes) minBytes = f.bytes;
      if (f.bytes > maxBytes) maxBytes = f.bytes;
    }
    // axes snap to data range, only guard is ≥1 for log math. svg padding
    // keeps extreme points off the border
    return {
      minAge: Math.max(1, minAge),
      maxAge: Math.max(minAge + 1, maxAge),
      minBytes: Math.max(1, minBytes),
      maxBytes: Math.max(minBytes + 1, maxBytes),
    };
  });

  // one tick per log decade per axis, no chart lib needed
  const xTicks = createMemo(() => niceLogTicks(bounds().minAge, bounds().maxAge));
  const yTicks = createMemo(() => niceLogTicks(bounds().minBytes, bounds().maxBytes));

  const hoveredFile = createMemo(() => {
    const h = props.hovered;
    if (!h) return null;
    return props.files.find((f) => f.path === h) ?? null;
  });

  return (
    <div
      class="safai-card"
      style={{
        padding: '18px 22px 22px',
        'flex-shrink': 0,
      }}
    >
      <div
        style={{
          display: 'flex',
          'align-items': 'baseline',
          'justify-content': 'space-between',
          'margin-bottom': '10px',
        }}
      >
        <div>
          <div
            style={{
              'font-size': '10px',
              color: 'var(--safai-fg-3)',
              'letter-spacing': '0.12em',
              'text-transform': 'uppercase',
              'margin-bottom': '2px',
            }}
          >
            Size × Age
          </div>
          <div style={{ 'font-size': '13px', color: 'var(--safai-fg-1)' }}>
            <span class="num">{formatCount(props.files.length)}</span> points ·
            bigger up, older right. Click a point to toggle selection.
          </div>
        </div>
      </div>

      <PlotCanvas
        files={props.files}
        bounds={bounds()}
        xTicks={xTicks()}
        yTicks={yTicks()}
        selected={props.selected}
        hovered={props.hovered}
        hoveredFile={hoveredFile()}
        onHover={props.onHover}
        onToggle={props.onToggle}
      />

      <div
        style={{
          display: 'flex',
          'justify-content': 'space-between',
          'margin-top': '4px',
          'font-size': '10px',
          color: 'var(--safai-fg-3)',
        }}
      >
        <span>
          Idle:{' '}
          <span class="num">{formatCount(Math.max(0, bounds().minAge))}</span>d →{' '}
          <span class="num">{formatCount(bounds().maxAge)}</span>d
        </span>
        <span>
          Size: <span class="num">{formatBytes(bounds().minBytes)}</span> →{' '}
          <span class="num">{formatBytes(bounds().maxBytes)}</span>
        </span>
      </div>
    </div>
  );
}

// log-spaced ticks w/ endpoints included, so labels read "182d ... 2,134d"
// instead of "100d ... 10,000d" which used to push the right edge past the
// data and squish the plot to the left third
function niceLogTicks(min: number, max: number): number[] {
  const safeMin = Math.max(1, min);
  const safeMax = Math.max(safeMin, max);
  if (safeMax === safeMin) return [safeMin];
  const lo = Math.log10(safeMin);
  const hi = Math.log10(safeMax);
  // ~5 ticks, at least 2 (endpoints). tiny ranges drop the intermediates
  // so ticks don't pile up
  const spanDecades = hi - lo;
  const n = spanDecades < 0.5 ? 2 : spanDecades < 1.2 ? 3 : spanDecades < 2.5 ? 4 : 5;
  const ticks: number[] = [];
  for (let i = 0; i < n; i++) {
    const frac = i / (n - 1);
    ticks.push(Math.round(Math.pow(10, lo + spanDecades * frac)));
  }
  // dedupe rounding collisions
  return Array.from(new Set(ticks));
}

// positions in %, sizes in px. % makes it responsive without JS width
// tracking, px keeps the circles circular when aspect changes
const POINT_R = 5;
const POINT_HOVER_R = 7;
const POINT_SEL_R = 6;

interface PlotCanvasProps {
  files: FileSummary[];
  bounds: { minAge: number; maxAge: number; minBytes: number; maxBytes: number };
  xTicks: number[];
  yTicks: number[];
  selected: Set<string>;
  hovered: string | null;
  hoveredFile: FileSummary | null;
  onHover: (path: string | null) => void;
  onToggle: (path: string) => void;
}

// pure-css scatter. everything is left/top %, no js measurement. the old
// svg version fought ResizeObserver + missed the initial layout pass under
// the tauri webview. points are absolute divs w/ fixed px radii so circles
// stay circular at any aspect. grid layout: axis label columns/rows auto-
// size to content, plot fills the middle cell.
function PlotCanvas(props: PlotCanvasProps) {
  return (
    <div
      style={{
        display: 'grid',
        'grid-template-columns': 'auto 1fr',
        'grid-template-rows': '1fr auto',
        gap: '6px 8px',
        width: '100%',
        // aspect-ratio = smooth height-tracks-width, no JS. min/max-height
        // clamps handle extreme narrow/wide where 2.2:1 looks silly
        'aspect-ratio': '2.4 / 1',
        'min-height': '320px',
        'max-height': '560px',
      }}
    >
      {/* y-axis labels */}
      <div
        style={{
          position: 'relative',
          width: '36px',
          'min-width': '36px',
        }}
      >
        <For each={props.yTicks}>
          {(tick) => {
            const ny = logNormValue(tick, props.bounds.minBytes, props.bounds.maxBytes);
            return (
              <div
                style={{
                  position: 'absolute',
                  right: '2px',
                  top: `${(1 - ny) * 100}%`,
                  transform: 'translateY(-50%)',
                  'font-size': '10px',
                  color: 'var(--safai-fg-3)',
                  'white-space': 'nowrap',
                  'font-variant-numeric': 'tabular-nums',
                }}
              >
                {formatBytes(tick)}
              </div>
            );
          }}
        </For>
      </div>

      {/* plot area */}
      <div
        style={{
          position: 'relative',
          background: 'var(--safai-bg-1)',
          border: '1px solid var(--safai-line)',
          'border-radius': 'var(--safai-r-sm)',
          overflow: 'hidden',
        }}
      >
        {/* y gridlines */}
        <For each={props.yTicks}>
          {(tick) => {
            const ny = logNormValue(tick, props.bounds.minBytes, props.bounds.maxBytes);
            return (
              <div
                style={{
                  position: 'absolute',
                  left: 0,
                  right: 0,
                  top: `${(1 - ny) * 100}%`,
                  'border-top': '1px dashed var(--safai-line)',
                  opacity: 0.4,
                  'pointer-events': 'none',
                }}
              />
            );
          }}
        </For>

        {/* x gridlines */}
        <For each={props.xTicks}>
          {(tick) => {
            const nx = logNormValue(tick, props.bounds.minAge, props.bounds.maxAge);
            return (
              <div
                style={{
                  position: 'absolute',
                  top: 0,
                  bottom: 0,
                  left: `${nx * 100}%`,
                  'border-left': '1px dashed var(--safai-line)',
                  opacity: 0.4,
                  'pointer-events': 'none',
                }}
              />
            );
          }}
        </For>

        {/* points */}
        <For each={props.files}>
          {(f) => {
            const nx = logNormValue(f.idleDays, props.bounds.minAge, props.bounds.maxAge);
            const ny = logNormValue(f.bytes, props.bounds.minBytes, props.bounds.maxBytes);
            const isSel = props.selected.has(f.path);
            const isHov = props.hovered === f.path;
            const colour = bucketColour(bucketFor(f.extension));
            const r = isHov ? POINT_HOVER_R : isSel ? POINT_SEL_R : POINT_R;
            return (
              <div
                role="button"
                tabindex={-1}
                title={`${f.path} · ${formatBytes(f.bytes)} · ${formatCount(f.idleDays)} days idle`}
                onMouseEnter={() => props.onHover(f.path)}
                onMouseLeave={() => props.onHover(null)}
                onClick={() => props.onToggle(f.path)}
                style={{
                  position: 'absolute',
                  left: `${nx * 100}%`,
                  top: `${(1 - ny) * 100}%`,
                  width: `${r * 2}px`,
                  height: `${r * 2}px`,
                  'border-radius': '50%',
                  background: colour,
                  opacity: isSel ? 0.95 : 0.78,
                  border: isSel
                    ? '1.5px solid var(--safai-fg-0)'
                    : isHov
                      ? `1px solid ${colour}`
                      : '1px solid transparent',
                  // -50% so centre sits at (left, top), otherwise points
                  // at the axes half-clip
                  transform: 'translate(-50%, -50%)',
                  cursor: 'pointer',
                  transition: 'width 0.1s ease, height 0.1s ease',
                  'box-sizing': 'border-box',
                  'z-index': isHov ? 3 : isSel ? 2 : 1,
                }}
              />
            );
          }}
        </For>

        {/* hover tooltip */}
        <Show when={props.hoveredFile}>
          {(f) => (
            <div
              style={{
                position: 'absolute',
                top: '8px',
                right: '10px',
                padding: '8px 12px',
                background: 'var(--safai-bg-2)',
                border: '1px solid var(--safai-line)',
                'border-radius': 'var(--safai-r-sm)',
                'box-shadow': '0 6px 16px oklch(0 0 0 / 0.35)',
                'max-width': 'min(420px, 70%)',
                'font-size': '11px',
                'pointer-events': 'none',
                'z-index': 10,
              }}
            >
              <div
                class="mono"
                style={{
                  'white-space': 'nowrap',
                  overflow: 'hidden',
                  'text-overflow': 'ellipsis',
                  color: 'var(--safai-fg-0)',
                  'margin-bottom': '4px',
                }}
              >
                {f().path}
              </div>
              <div style={{ color: 'var(--safai-fg-2)' }}>
                <span class="num">{formatBytes(f().bytes)}</span>
                {' · '}
                <span class="num">{formatCount(f().idleDays)}</span> days idle
                {' · '}
                {bucketLabel(bucketFor(f().extension))}
              </div>
            </div>
          )}
        </Show>
      </div>

      {/* corner spacer */}
      <div />

      {/* x-axis labels */}
      <div style={{ position: 'relative', height: '20px' }}>
        <For each={props.xTicks}>
          {(tick) => {
            const nx = logNormValue(tick, props.bounds.minAge, props.bounds.maxAge);
            return (
              <div
                style={{
                  position: 'absolute',
                  left: `${nx * 100}%`,
                  top: '2px',
                  transform: 'translateX(-50%)',
                  'font-size': '10px',
                  color: 'var(--safai-fg-3)',
                  'white-space': 'nowrap',
                  'font-variant-numeric': 'tabular-nums',
                }}
              >
                {formatCount(tick)}d
              </div>
            );
          }}
        </For>
      </div>
    </div>
  );
}

function logNormValue(value: number, min: number, max: number): number {
  const lo = Math.log10(Math.max(1, min));
  const hi = Math.log10(Math.max(lo + 0.0001, max));
  if (hi === lo) return 0.5;
  const v = Math.log10(Math.max(1, value));
  return Math.max(0, Math.min(1, (v - lo) / (hi - lo)));
}

// bucket legend / filter

function BucketLegend(props: {
  files: FileSummary[];
  filter: Bucket | 'all';
  onSelect: (bucket: Bucket | 'all') => void;
}) {
  const byBucket = createMemo(() => {
    const m = new Map<Bucket, { count: number; bytes: number }>();
    for (const f of props.files) {
      const b = bucketFor(f.extension);
      const entry = m.get(b) ?? { count: 0, bytes: 0 };
      entry.count++;
      entry.bytes += f.bytes;
      m.set(b, entry);
    }
    return Array.from(m.entries()).sort((a, b) => b[1].bytes - a[1].bytes);
  });
  return (
    <div
      style={{
        display: 'flex',
        'flex-wrap': 'wrap',
        gap: '8px',
        'align-items': 'center',
      }}
    >
      <LegendChip
        label="All"
        colour="var(--safai-fg-2)"
        active={props.filter === 'all'}
        onClick={() => props.onSelect('all')}
        caption={`${formatCount(props.files.length)} files`}
      />
      <For each={byBucket()}>
        {([bucket, info]) => (
          <LegendChip
            label={bucketLabel(bucket)}
            colour={bucketColour(bucket)}
            active={props.filter === bucket}
            onClick={() =>
              props.onSelect(props.filter === bucket ? 'all' : bucket)
            }
            caption={`${formatCount(info.count)} · ${formatBytes(info.bytes)}`}
          />
        )}
      </For>
    </div>
  );
}

function LegendChip(props: {
  label: string;
  colour: string;
  active: boolean;
  onClick: () => void;
  caption: string;
}) {
  return (
    <button
      type="button"
      class="safai-btn safai-btn--ghost"
      onClick={props.onClick}
      style={{
        height: '26px',
        padding: '0 10px',
        'border-radius': '999px',
        'font-size': '11px',
        gap: '6px',
        'background-color': props.active ? 'var(--safai-bg-3)' : undefined,
        'border-color': props.active ? props.colour : undefined,
      }}
    >
      <span
        aria-hidden="true"
        style={{
          display: 'inline-block',
          width: '8px',
          height: '8px',
          'border-radius': '50%',
          background: props.colour,
        }}
      />
      <span>{props.label}</span>
      <span style={{ color: 'var(--safai-fg-3)', 'font-size': '10px' }}>
        {props.caption}
      </span>
    </button>
  );
}

// table

function TableToolbar(props: {
  total: number;
  selectedCount: number;
  onSelectAll: () => void;
  onClear: () => void;
}) {
  return (
    <div
      style={{
        display: 'flex',
        'align-items': 'center',
        gap: '10px',
        'font-size': '11px',
        color: 'var(--safai-fg-2)',
      }}
    >
      <span>
        <span class="num" style={{ color: 'var(--safai-fg-0)' }}>
          {formatCount(props.selectedCount)}
        </span>{' '}
        of {formatCount(props.total)} selected
      </span>
      <button class="safai-btn safai-btn--ghost" onClick={props.onSelectAll}>
        Select all
      </button>
      <button
        class="safai-btn safai-btn--ghost"
        onClick={props.onClear}
        disabled={props.selectedCount === 0}
      >
        Clear
      </button>
    </div>
  );
}

function FilesTable(props: {
  files: FileSummary[];
  selected: Set<string>;
  sortField: SortField;
  sortDir: SortDir;
  onSort: (field: SortField) => void;
  onToggle: (path: string) => void;
  onReveal: (path: string) => void;
  onHover: (path: string | null) => void;
  hovered: string | null;
}) {
  const arrow = (f: SortField) =>
    props.sortField === f ? (props.sortDir === 'asc' ? '↑' : '↓') : '';
  return (
    <div class="safai-card" style={{ overflow: 'hidden', 'flex-shrink': 0 }}>
      <div
        style={{
          display: 'grid',
          'grid-template-columns': '32px 1fr 110px 90px 130px 90px',
          gap: '0',
          'border-bottom': '1px solid var(--safai-line)',
          background: 'oklch(0.18 0.008 240)',
          padding: '8px 14px',
          'font-size': '10px',
          color: 'var(--safai-fg-3)',
          'letter-spacing': '0.08em',
          'text-transform': 'uppercase',
        }}
      >
        <span></span>
        <HeaderCell
          label={`Path ${arrow('path')}`}
          onClick={() => props.onSort('path')}
          align="left"
        />
        <HeaderCell
          label={`Size ${arrow('bytes')}`}
          onClick={() => props.onSort('bytes')}
          align="right"
        />
        <HeaderCell
          label={`Kind ${arrow('extension')}`}
          onClick={() => props.onSort('extension')}
          align="left"
        />
        <HeaderCell
          label={`Idle days ${arrow('idleDays')}`}
          onClick={() => props.onSort('idleDays')}
          align="right"
        />
        <span style={{ 'text-align': 'right' }}>Actions</span>
      </div>
      <For each={props.files}>
        {(f) => {
          const isSel = () => props.selected.has(f.path);
          const isHov = () => props.hovered === f.path;
          const bucket = bucketFor(f.extension);
          return (
            <div
              style={{
                display: 'grid',
                'grid-template-columns': '32px 1fr 110px 90px 130px 90px',
                'align-items': 'center',
                padding: '10px 14px',
                'border-bottom': '1px solid var(--safai-line)',
                background: isSel()
                  ? 'color-mix(in oklab, var(--safai-cyan) 8%, transparent)'
                  : isHov()
                    ? 'var(--safai-bg-2)'
                    : 'transparent',
                cursor: 'pointer',
              }}
              onClick={() => props.onToggle(f.path)}
              onMouseEnter={() => props.onHover(f.path)}
              onMouseLeave={() => props.onHover(null)}
            >
              <div
                class={`safai-check safai-check--${isSel() ? 'on' : 'off'}`}
                role="checkbox"
                aria-checked={isSel()}
                tabindex={0}
                onKeyDown={(e) => {
                  if (e.key === ' ' || e.key === 'Enter') {
                    e.preventDefault();
                    props.onToggle(f.path);
                  }
                }}
              >
                <Show when={isSel()}>
                  <Icon
                    name="check"
                    size={9}
                    color="oklch(0.18 0.02 240)"
                    strokeWidth={2.2}
                  />
                </Show>
              </div>
              <div
                class="mono"
                style={{
                  'font-size': '12px',
                  color: 'var(--safai-fg-1)',
                  'min-width': 0,
                  'white-space': 'nowrap',
                  overflow: 'hidden',
                  'text-overflow': 'ellipsis',
                }}
                title={f.path}
              >
                {truncateMiddle(f.path, 80)}
              </div>
              <div
                class="num"
                style={{
                  'font-size': '12px',
                  'text-align': 'right',
                  'font-variant-numeric': 'tabular-nums',
                  color: 'var(--safai-fg-0)',
                }}
              >
                {formatBytes(f.bytes)}
              </div>
              <div style={{ 'font-size': '11px', color: 'var(--safai-fg-2)' }}>
                <span
                  aria-hidden="true"
                  style={{
                    display: 'inline-block',
                    width: '6px',
                    height: '6px',
                    'border-radius': '50%',
                    background: bucketColour(bucket),
                    'margin-right': '6px',
                    'vertical-align': 'middle',
                  }}
                />
                {f.extension ? f.extension : bucketLabel(bucket).toLowerCase()}
              </div>
              <div
                class="num"
                style={{
                  'font-size': '12px',
                  'text-align': 'right',
                  color: 'var(--safai-fg-1)',
                  'font-variant-numeric': 'tabular-nums',
                }}
              >
                {formatCount(f.idleDays)} d
              </div>
              <div
                style={{
                  display: 'flex',
                  'justify-content': 'flex-end',
                  gap: '6px',
                }}
                onClick={(e) => e.stopPropagation()}
              >
                <button
                  class="safai-btn safai-btn--ghost"
                  title="Reveal in file manager"
                  onClick={() => props.onReveal(f.path)}
                  style={{ height: '22px', padding: '0 6px' }}
                >
                  <Icon name="eye" size={11} />
                </button>
              </div>
            </div>
          );
        }}
      </For>
    </div>
  );
}

function HeaderCell(props: {
  label: string;
  onClick: () => void;
  align?: 'left' | 'right';
}) {
  return (
    <button
      type="button"
      onClick={props.onClick}
      class="safai-btn safai-btn--ghost"
      style={{
        height: '20px',
        padding: '0 4px',
        'font-size': '10px',
        'font-weight': 500,
        'letter-spacing': '0.08em',
        'text-transform': 'uppercase',
        color: 'var(--safai-fg-3)',
        background: 'transparent',
        border: 'none',
        'justify-content': props.align === 'right' ? 'flex-end' : 'flex-start',
      }}
    >
      {props.label}
    </button>
  );
}

// aux cards

function EmptyState() {
  return (
    <div
      class="safai-card"
      style={{
        padding: '40px 32px',
        display: 'flex',
        gap: '20px',
        'align-items': 'center',
      }}
    >
      <Suds size={72} mood="happy" />
      <div style={{ flex: 1 }}>
        <div style={{ 'font-size': '15px', 'font-weight': 500, 'margin-bottom': '4px' }}>
          No large, stale files found.
        </div>
        <div style={{ 'font-size': '12px', color: 'var(--safai-fg-2)', 'line-height': 1.5 }}>
          Suds walked your home directory looking for files ≥ 50 MB that you
          haven't touched in 6 months - and didn't find any worth surfacing.
          Nicely maintained.
        </div>
      </div>
    </div>
  );
}

function ScanningCard(props: { response: LargeOldReport | null }) {
  const phase = () => props.response?.phase;
  const label = () => (phase() ? phaseLabel(phase()!) : 'Starting scan');
  const files = () => props.response?.totalFilesScanned ?? 0;
  return (
    <div
      class="safai-card"
      style={{
        padding: '22px 26px',
        display: 'flex',
        'align-items': 'center',
        gap: '18px',
        border: '1px solid oklch(0.82 0.14 200 / 0.3)',
        background:
          'linear-gradient(90deg, var(--safai-bg-2) 0%, var(--safai-bg-3) 50%, var(--safai-bg-2) 100%)',
        'background-size': '200% 100%',
        animation: 'safai-shimmer 2.2s ease-in-out infinite',
      }}
    >
      <Suds size={48} mood="happy" float />
      <div style={{ flex: 1, 'min-width': 0 }}>
        <div
          style={{
            'font-size': '10px',
            color: 'var(--safai-cyan)',
            'letter-spacing': '0.12em',
            'text-transform': 'uppercase',
            'margin-bottom': '4px',
          }}
        >
          {label()}
        </div>
        <div style={{ 'font-size': '13px', color: 'var(--safai-fg-1)' }}>
          Walked{' '}
          <span class="num" style={{ color: 'var(--safai-fg-0)' }}>
            {formatCount(files())}
          </span>{' '}
          files
        </div>
      </div>
    </div>
  );
}

function ErrorCard(props: { message: string; onRetry: () => void }) {
  return (
    <div
      class="safai-card"
      style={{
        padding: '18px 22px',
        display: 'flex',
        'align-items': 'center',
        gap: '16px',
        border: '1px solid oklch(0.68 0.18 25 / 0.4)',
      }}
    >
      <Suds size={56} mood="shocked" />
      <div style={{ flex: 1 }}>
        <div style={{ 'font-size': '13px', color: 'var(--safai-fg-0)', 'margin-bottom': '2px' }}>
          Couldn't run the scan
        </div>
        <div class="mono" style={{ 'font-size': '11px', color: 'var(--safai-fg-2)' }}>
          {props.message}
        </div>
      </div>
      <button class="safai-btn safai-btn--ghost" onClick={props.onRetry}>
        <Icon name="refresh" size={12} /> Try again
      </button>
    </div>
  );
}

function StatusBanner(props: {
  error: string | null;
  commit: DeleteResult | null;
  restore: string | null;
  onDismiss: () => void;
}) {
  const tone = () => (props.error ? 'error' : 'ok');
  const text = () => {
    if (props.error) return `Couldn't prepare cleanup: ${props.error}`;
    if (props.commit) {
      const n = props.commit.committed.length;
      const failed = props.commit.failed.length;
      const base = `Cleaned ${formatCount(n)} file${n === 1 ? '' : 's'} · ${formatBytes(
        props.commit.bytesTrashed,
      )} freed`;
      return failed > 0 ? `${base} · ${formatCount(failed)} failed` : base;
    }
    return props.restore ?? '';
  };
  return (
    <div
      role="status"
      aria-live="polite"
      style={{
        position: 'fixed',
        bottom: '20px',
        left: '50%',
        transform: 'translateX(-50%)',
        padding: '10px 16px',
        'border-radius': 'var(--safai-r-md)',
        background:
          tone() === 'error' ? 'var(--safai-coral-dim)' : 'var(--safai-cyan-dim)',
        color: tone() === 'error' ? 'var(--safai-coral)' : 'var(--safai-cyan)',
        'font-size': '12px',
        display: 'flex',
        'align-items': 'center',
        gap: '10px',
        'z-index': 500,
        'box-shadow': '0 12px 32px oklch(0 0 0 / 0.4)',
      }}
    >
      <Icon
        name={tone() === 'error' ? 'warning' : 'check'}
        size={12}
        color="currentColor"
      />
      <span>{text()}</span>
      <button
        class="safai-btn safai-btn--ghost"
        style={{ height: '24px', 'font-size': '11px', padding: '0 8px' }}
        onClick={props.onDismiss}
      >
        Dismiss
      </button>
    </div>
  );
}
