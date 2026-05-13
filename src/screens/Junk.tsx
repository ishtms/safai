import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon, type IconName } from '../components/Icon';
import { ConfirmDeleteModal } from '../components/ConfirmDeleteModal';
import { CacheFreshness } from '../components/CacheFreshness';
import {
  junkScan,
  type JunkCategoryReport,
  type JunkPathDetail,
  type JunkReport,
} from '../lib/junk';
import { CACHE_JUNK, KEY_JUNK, sharedResource } from '../lib/scanCache';
import {
  commitDelete,
  graveyardStats,
  previewDelete,
  purgeGraveyard,
  restoreLast,
  type DeletePlan,
  type DeleteResult,
  type GraveyardStats,
} from '../lib/cleaner';
import { invalidateFilesystemCachesSoon } from '../lib/cacheInvalidation';
import { formatBytes, formatCount, formatRelativeTime, truncateMiddle } from '../lib/format';
import { useFlashMood } from '../lib/moods';
import type { SudsMood } from '../components/Suds';

// system junk. category cards on the left expand to per-path rows w/ checkboxes,
// detail pane on the right (suds + preview)
export default function Junk() {
  // shared cache so tab-switching doesn't re-run the scan. rescan button
  // still forces a fresh fetch via refetch()
  const [report, { refetch }] = sharedResource(CACHE_JUNK, junkScan);
  const [expanded, setExpanded] = createSignal<Set<string>>(new Set(['user-caches']));
  const [focusedPath, setFocusedPath] = createSignal<{ cat: JunkCategoryReport; row: JunkPathDetail } | null>(null);
  const navigate = useNavigate();

  // suds winks for ~2s after clean/restore then settles. purely visual
  const sudsMood = useFlashMood({
    base: () => 'happy' as SudsMood,
    transient: 'wink' as SudsMood,
  });

  // drives relative timestamp re-renders. 250ms is snappy without burning
  // cycles idle
  const [clock, setClock] = createSignal(Date.now());
  onMount(() => {
    const clockTick = window.setInterval(() => setClock(Date.now()), 250);
    onCleanup(() => window.clearInterval(clockTick));
  });

  // rescan routes through the shared /scanning progress screen rather than
  // a silent refetch. ?kind=junk makes it run junk_scan then nav back here,
  // remount re-fetches the resource with fresh numbers
  const doRescan = () => {
    navigate('/scanning?kind=junk');
  };
  const isBusy = () => report.loading;

  // start fully-selected. the effect below reseeds on each fresh report but
  // preserves manual toggles after
  const [selectedCats, setSelectedCats] = createSignal<Set<string>>(new Set());
  let lastSeenReportStamp: number | null = null;
  createEffect(() => {
    const r = report();
    if (!r) return;
    // scannedAt doubles as "did report change" token. refetch bumps it,
    // manual toggles don't
    if (r.scannedAt === lastSeenReportStamp) return;
    lastSeenReportStamp = r.scannedAt;
    setSelectedCats(new Set(r.categories.filter((c) => c.available).map((c) => c.id)));
  });
  const availableCats = () => report()?.categories.filter((c) => c.available) ?? [];

  const selectedBytes = createMemo(() => {
    const r = report();
    if (!r) return 0;
    const s = selectedCats();
    return r.categories.filter((c) => s.has(c.id)).reduce((acc, c) => acc + c.bytes, 0);
  });
  const totalAvailableBytes = () => availableCats().reduce((acc, c) => acc + c.bytes, 0);

  const toggleExpanded = (id: string) => {
    const s = new Set(expanded());
    if (s.has(id)) s.delete(id);
    else s.add(id);
    setExpanded(s);
  };
  const toggleCatSelected = (id: string) => {
    const s = new Set(selectedCats());
    if (s.has(id)) s.delete(id);
    else s.add(id);
    setSelectedCats(s);
  };

  // delete flow: selected cats -> previewDelete (rust returns DeletePlan w/
  // token) -> ConfirmDeleteModal -> commitDelete(token) moves files to the
  // safai graveyard. failures surface inline
  const [pendingPlan, setPendingPlan] = createSignal<DeletePlan | null>(null);
  const [planError, setPlanError] = createSignal<string | null>(null);
  const [committing, setCommitting] = createSignal(false);
  const [commitResult, setCommitResult] = createSignal<DeleteResult | null>(null);
  const [restoring, setRestoring] = createSignal(false);
  const [restoreMessage, setRestoreMessage] = createSignal<string | null>(null);

  // drives "Graveyard: X" readout and whether Empty is clickable.
  // refetched after commit/restore/purge so no timer needed
  const [stats, { refetch: refetchStats }] = createResource<GraveyardStats>(graveyardStats);
  const [purging, setPurging] = createSignal(false);
  const [confirmPurge, setConfirmPurge] = createSignal(false);
  const doPurge = async () => {
    if (purging()) return;
    setPurging(true);
    try {
      const r = await purgeGraveyard();
      setRestoreMessage(
        `Emptied Safai trash - freed ${formatBytes(r.bytesFreed)} across ${formatCount(
          r.purged.length,
        )} batch${r.purged.length === 1 ? '' : 'es'}.`,
      );
      invalidateFilesystemCachesSoon(KEY_JUNK);
      refetch();
    } catch (e) {
      setRestoreMessage(`Couldn't empty trash: ${String(e)}`);
    } finally {
      setConfirmPurge(false);
      setPurging(false);
      refetchStats();
    }
  };

  const selectedPaths = createMemo<string[]>(() => {
    const r = report();
    if (!r) return [];
    const s = selectedCats();
    // expand cats into per-path rows, the cleaner wants real fs paths not
    // synthetic category ids. rust scanner guarantees absolute
    const out: string[] = [];
    for (const cat of r.categories) {
      if (!s.has(cat.id) || !cat.available) continue;
      for (const row of cat.paths) out.push(row.path);
    }
    return out;
  });

  const openConfirm = async () => {
    setPlanError(null);
    setCommitResult(null);
    const paths = selectedPaths();
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
      sudsMood.flash();
      invalidateFilesystemCachesSoon(KEY_JUNK);
      // rescan so emptied cats drop to zero
      refetch();
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
      if (n > 0) sudsMood.flash();
      invalidateFilesystemCachesSoon(KEY_JUNK);
      refetch();
      refetchStats();
    } catch (e) {
      setRestoreMessage(`Couldn't restore: ${String(e)}`);
    } finally {
      setRestoring(false);
    }
  };

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Cleanup"
        title="System Junk"
        subtitle="Caches, logs, and files nothing will miss."
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
              onClick={doRescan}
              disabled={isBusy()}
              aria-label="Rescan junk"
              aria-busy={isBusy()}
            >
              <span class={isBusy() ? 'safai-spin' : ''} style={{ display: 'inline-flex' }}>
                <Icon name="refresh" size={12} />
              </span>{' '}
              {isBusy() ? 'Scanning…' : 'Rescan'}
            </button>
            <button
              class="safai-btn safai-btn--ghost"
              onClick={doRestore}
              disabled={restoring() || isBusy()}
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
              disabled={
                isBusy() ||
                committing() ||
                selectedBytes() === 0 ||
                selectedPaths().length === 0
              }
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
          padding: '24px',
          display: 'grid',
          'grid-template-columns': '1fr 320px',
          gap: '20px',
        }}
      >
        <div>
          <Show when={report.error}>
            <ErrorCard message={String(report.error)} onRetry={() => refetch()} />
          </Show>

          <Show when={!report.error}>
            <SelectAllBar
              total={totalAvailableBytes()}
              selected={selectedBytes()}
              loading={report.loading && !report()}
            />

            <Show
              when={!report.loading || report()}
              fallback={<For each={Array.from({ length: 5 })}>{() => <SkeletonRow />}</For>}
            >
              <For each={report()?.categories ?? []}>
                {(cat) => (
                  <CategoryRow
                    cat={cat}
                    expanded={expanded().has(cat.id)}
                    selected={selectedCats().has(cat.id)}
                    focusedPath={focusedPath()?.row?.path ?? null}
                    onToggleExpanded={() => toggleExpanded(cat.id)}
                    onToggleSelected={() => toggleCatSelected(cat.id)}
                    onFocusRow={(row) => setFocusedPath({ cat, row })}
                  />
                )}
              </For>
            </Show>

            <Show when={report()}>
              {(r) => (
                <div
                  style={{
                    'margin-top': '18px',
                    'font-size': '11px',
                    color: 'var(--safai-fg-3)',
                    display: 'flex',
                    gap: '12px',
                    'align-items': 'center',
                  }}
                  aria-live="polite"
                >
                  <Show when={isBusy()} fallback={
                    <span>
                      Last scanned {formatRelativeTime(r().scannedAt, clock())}
                    </span>
                  }>
                    <span style={{ color: 'var(--safai-cyan)' }}>
                      <span class="safai-spin" style={{ display: 'inline-block', 'margin-right': '6px' }}>
                        <Icon name="refresh" size={11} color="var(--safai-cyan)" />
                      </span>
                      Scanning…
                    </span>
                  </Show>
                  <span>·</span>
                  <span>Scanned in {Math.max(1, Math.round(r().durationMs))} ms</span>
                  <span>·</span>
                  <span>{formatCount(r().totalItems)} items catalogued</span>
                  <span>·</span>
                  <span>Platform: {r().platform}</span>
                  <span>·</span>
                  <CacheFreshness
                    cacheKey={CACHE_JUNK}
                    version={r()}
                    disabled={isBusy()}
                    onRescan={doRescan}
                  />
                  <Show when={(stats()?.batchCount ?? 0) > 0}>
                    <span>·</span>
                    <span>
                      Graveyard:{' '}
                      <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
                        {formatBytes(stats()?.totalBytes ?? 0)}
                      </span>{' '}
                      ({formatCount(stats()?.batchCount ?? 0)} batch
                      {(stats()?.batchCount ?? 0) === 1 ? '' : 'es'})
                    </span>
                    <button
                      class="safai-btn safai-btn--ghost"
                      style={{ height: '22px', 'font-size': '11px', padding: '0 8px' }}
                      onClick={() => setConfirmPurge(true)}
                      disabled={purging() || restoring() || isBusy()}
                      title="Permanently delete everything in Safai's trash"
                    >
                      <Icon name="trash" size={10} /> Empty
                    </button>
                  </Show>
                </div>
              )}
            </Show>
          </Show>
        </div>

        <DetailPane focused={focusedPath()} report={report()} mood={sudsMood.mood()} />
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

      <Show when={confirmPurge()}>
        <PurgeConfirmModal
          stats={stats()}
          purging={purging()}
          onCancel={() => {
            if (!purging()) setConfirmPurge(false);
          }}
          onConfirm={doPurge}
        />
      </Show>
    </div>
  );
}

function PurgeConfirmModal(props: {
  stats: GraveyardStats | undefined;
  purging: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const bytes = () => props.stats?.totalBytes ?? 0;
  const count = () => props.stats?.batchCount ?? 0;
  const handleKey = (e: KeyboardEvent) => {
    if (props.purging) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      props.onCancel();
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      props.onConfirm();
    }
  };
  return (
    <div
      style={{
        position: 'fixed',
        inset: 0,
        'z-index': 1000,
        display: 'flex',
        'align-items': 'center',
        'justify-content': 'center',
        background: 'oklch(0 0 0 / 0.55)',
        'backdrop-filter': 'blur(4px)',
        'padding': '24px',
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget && !props.purging) props.onCancel();
      }}
      onKeyDown={handleKey}
      role="dialog"
      aria-modal="true"
    >
      <div
        class="safai-card"
        style={{
          width: '100%',
          'max-width': '460px',
          padding: '24px 28px',
          background: 'var(--safai-bg-1)',
          'box-shadow': '0 24px 80px oklch(0 0 0 / 0.6)',
        }}
      >
        <div style={{ display: 'flex', gap: '16px', 'align-items': 'flex-start' }}>
          <Suds size={48} mood="shocked" />
          <div style={{ flex: 1 }}>
            <div style={{ 'font-size': '16px', 'font-weight': 600, 'margin-bottom': '4px' }}>
              Empty Safai trash?
            </div>
            <div style={{ 'font-size': '12px', color: 'var(--safai-fg-2)', 'line-height': 1.5 }}>
              Permanently deletes{' '}
              <span class="num" style={{ color: 'var(--safai-fg-0)' }}>
                {formatBytes(bytes())}
              </span>{' '}
              across {formatCount(count())} batch{count() === 1 ? '' : 'es'}. After
              this, Restore last won't be able to bring anything back.
            </div>
          </div>
        </div>
        <div
          style={{
            'margin-top': '20px',
            display: 'flex',
            'justify-content': 'flex-end',
            gap: '10px',
          }}
        >
          <button
            class="safai-btn safai-btn--ghost"
            onClick={props.onCancel}
            disabled={props.purging}
          >
            Cancel
          </button>
          <button
            class="safai-btn safai-btn--danger"
            onClick={props.onConfirm}
            disabled={props.purging}
            aria-busy={props.purging}
            autofocus
          >
            <span class={props.purging ? 'safai-spin' : ''} style={{ display: 'inline-flex' }}>
              <Icon name={props.purging ? 'refresh' : 'trash'} size={12} />
            </span>{' '}
            {props.purging ? 'Emptying…' : `Empty ${formatBytes(bytes())}`}
          </button>
        </div>
      </div>
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
      const base = `Cleaned ${formatCount(n)} item${n === 1 ? '' : 's'} · ${formatBytes(
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

// subcomponents

function SelectAllBar(props: { total: number; selected: number; loading: boolean }) {
  const state = () =>
    props.selected === 0 ? 'off' : props.selected >= props.total ? 'on' : 'mixed';
  return (
    <div
      style={{
        display: 'flex',
        'align-items': 'center',
        gap: '8px',
        'margin-bottom': '10px',
        padding: '0 4px',
      }}
    >
      <div
        class={`safai-check safai-check--${state()}`}
        aria-label={`Selection: ${state()}`}
      >
        <Show when={state() === 'on'}>
          <Icon name="check" size={9} color="oklch(0.18 0.02 240)" strokeWidth={2.2} />
        </Show>
      </div>
      <span style={{ 'font-size': '12px', color: 'var(--safai-fg-1)' }}>
        Select all ·{' '}
        <span class="num">{props.loading ? '-' : formatBytes(props.selected)}</span>{' '}
        of <span class="num">{formatBytes(props.total)}</span>
      </span>
      <div style={{ flex: 1 }} />
      <button class="safai-btn safai-btn--ghost" style={{ height: '28px', 'font-size': '12px' }}>
        Sort by size
      </button>
    </div>
  );
}

function CategoryRow(props: {
  cat: JunkCategoryReport;
  expanded: boolean;
  selected: boolean;
  focusedPath: string | null;
  onToggleExpanded: () => void;
  onToggleSelected: () => void;
  onFocusRow: (row: JunkPathDetail) => void;
}) {
  return (
    <div class="safai-card" style={{ 'margin-bottom': '8px', overflow: 'hidden' }}>
      <div
        style={{
          padding: '14px 16px',
          display: 'flex',
          'align-items': 'center',
          gap: '12px',
          cursor: props.cat.available ? 'pointer' : 'default',
          opacity: props.cat.available ? 1 : 0.5,
        }}
        onClick={() => props.cat.available && props.onToggleExpanded()}
      >
        <Icon
          name={props.expanded ? 'chevronD' : 'chevronR'}
          size={12}
          color="var(--safai-fg-2)"
        />
        <div
          class={`safai-check safai-check--${props.selected && props.cat.available ? 'on' : 'off'}`}
          onClick={(e) => {
            e.stopPropagation();
            if (props.cat.available) props.onToggleSelected();
          }}
          aria-label={`Select ${props.cat.label}`}
        >
          <Show when={props.selected && props.cat.available}>
            <Icon name="check" size={9} color="oklch(0.18 0.02 240)" strokeWidth={2.2} />
          </Show>
        </div>
        <div
          style={{
            width: '28px',
            height: '28px',
            'border-radius': '7px',
            background: 'color-mix(in oklab, var(--safai-cyan) 12%, transparent)',
            display: 'flex',
            'align-items': 'center',
            'justify-content': 'center',
          }}
        >
          <Icon name={props.cat.icon as IconName} size={14} color="var(--safai-cyan)" />
        </div>
        <div style={{ flex: 1, 'min-width': 0 }}>
          <div
            style={{
              'font-size': '14px',
              'font-weight': 500,
              display: 'flex',
              'align-items': 'center',
              gap: '8px',
            }}
          >
            {props.cat.label}
            <Show when={props.cat.hot}>
              <span
                class="safai-pill"
                style={{
                  background: 'var(--safai-amber-dim)',
                  color: 'var(--safai-amber)',
                  'font-size': '9px',
                }}
              >
                hot
              </span>
            </Show>
            <Show when={!props.cat.available}>
              <span
                class="safai-pill"
                style={{
                  background: 'var(--safai-bg-2)',
                  color: 'var(--safai-fg-3)',
                  'font-size': '9px',
                }}
              >
                not on this system
              </span>
            </Show>
          </div>
          <div style={{ 'font-size': '11px', color: 'var(--safai-fg-3)', 'margin-top': '2px' }}>
            {props.cat.available
              ? `${formatCount(props.cat.items)} items · ${props.cat.description}`
              : props.cat.description}
          </div>
        </div>
        <div
          class="num"
          style={{
            'font-size': '15px',
            'font-family': 'var(--safai-font-display)',
            'font-weight': 600,
            'font-variant-numeric': 'tabular-nums',
          }}
        >
          {formatBytes(props.cat.bytes)}
        </div>
      </div>
      <Show when={props.expanded && props.cat.paths.length > 0}>
        <div
          style={{
            'border-top': '1px solid var(--safai-line)',
            background: 'oklch(0.18 0.008 240)',
          }}
        >
          <For each={props.cat.paths}>
            {(row) => (
              <div
                style={{
                  padding: '10px 16px 10px 52px',
                  display: 'flex',
                  'align-items': 'center',
                  gap: '12px',
                  'border-bottom': '1px solid var(--safai-line)',
                  cursor: 'pointer',
                  background:
                    props.focusedPath === row.path ? 'var(--safai-cyan-dim)' : 'transparent',
                }}
                onClick={() => props.onFocusRow(row)}
              >
                <div
                  class="safai-check safai-check--on"
                  aria-hidden="true"
                >
                  <Icon name="check" size={9} color="oklch(0.18 0.02 240)" strokeWidth={2.2} />
                </div>
                <div style={{ flex: 1, 'min-width': 0 }}>
                  <div
                    class="mono"
                    style={{
                      'font-size': '12px',
                      color: 'var(--safai-fg-1)',
                      'white-space': 'nowrap',
                      overflow: 'hidden',
                      'text-overflow': 'ellipsis',
                    }}
                    title={row.path}
                  >
                    {truncateMiddle(row.path, 80)}
                  </div>
                  <div
                    style={{ 'font-size': '10px', color: 'var(--safai-fg-3)', 'margin-top': '2px' }}
                  >
                    {formatCount(row.fileCount)} files
                    <Show when={row.lastModified != null}>
                      {' · Last written '}
                      {formatRelativeTime(row.lastModified, Date.now())}
                    </Show>
                  </div>
                </div>
                <div class="num" style={{ 'font-size': '12px', color: 'var(--safai-fg-1)' }}>
                  {formatBytes(row.bytes)}
                </div>
              </div>
            )}
          </For>
          <Show when={props.cat.paths.length === 0 && props.cat.items > 0}>
            <div
              style={{
                padding: '12px 16px 12px 52px',
                'font-size': '11px',
                color: 'var(--safai-fg-3)',
              }}
            >
              Too many items to list individually - full roll-up shown above.
            </div>
          </Show>
        </div>
      </Show>
    </div>
  );
}

function DetailPane(props: {
  focused: { cat: JunkCategoryReport; row: JunkPathDetail } | null;
  report: JunkReport | undefined;
  mood: SudsMood;
}) {
  return (
    <div>
      <div class="safai-card" style={{ padding: '18px', 'margin-bottom': '12px' }}>
        <div
          style={{ display: 'flex', 'align-items': 'center', gap: '10px', 'margin-bottom': '12px' }}
        >
          <Suds size={36} mood={props.mood} />
          <div style={{ 'font-size': '12px', color: 'var(--safai-fg-1)', 'font-weight': 500 }}>
            Suds says
          </div>
        </div>
        <div
          style={{
            'font-size': '13px',
            color: 'var(--safai-fg-1)',
            'line-height': 1.5,
            'margin-bottom': '12px',
          }}
        >
          {sudsCopy(props.focused?.cat, props.report)}
        </div>
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-3)',
            'padding-top': '12px',
            'border-top': '1px solid var(--safai-line)',
          }}
        >
          Apps might start a little slower on next launch while caches rebuild.
        </div>
      </div>

      <Show
        when={props.focused}
        fallback={
          <div class="safai-card" style={{ padding: '18px' }}>
            <div
              style={{
                'font-size': '11px',
                color: 'var(--safai-fg-3)',
                'letter-spacing': '0.12em',
                'text-transform': 'uppercase',
                'margin-bottom': '12px',
              }}
            >
              Preview
            </div>
            <div style={{ 'font-size': '12px', color: 'var(--safai-fg-2)' }}>
              Select a row on the left to preview it.
            </div>
          </div>
        }
      >
        {(f) => <PreviewCard cat={f().cat} row={f().row} />}
      </Show>
    </div>
  );
}

function PreviewCard(props: { cat: JunkCategoryReport; row: JunkPathDetail }) {
  const now = Date.now();
  const lastSeen = () =>
    props.row.lastModified != null
      ? formatRelativeTime(props.row.lastModified, now)
      : 'Unknown';
  return (
    <div class="safai-card" style={{ padding: '18px' }}>
      <div
        style={{
          'font-size': '11px',
          color: 'var(--safai-fg-3)',
          'letter-spacing': '0.12em',
          'text-transform': 'uppercase',
          'margin-bottom': '12px',
        }}
      >
        Preview
      </div>
      <div style={{ 'font-size': '13px', 'font-weight': 500, 'margin-bottom': '4px' }}>
        {props.cat.label}
      </div>
      <div
        class="mono"
        title={props.row.path}
        style={{
          'font-size': '10px',
          color: 'var(--safai-fg-2)',
          'margin-bottom': '14px',
          'word-break': 'break-all',
          'max-height': '60px',
          'overflow-y': 'auto',
          'line-height': 1.4,
        }}
      >
        {props.row.path}
      </div>
      <For
        each={[
          ['Size', formatBytes(props.row.bytes)],
          ['Files', formatCount(props.row.fileCount)],
          ['Last used', lastSeen()],
          ['Safety', props.cat.available ? 'Safe to delete' : 'Not present'],
        ]}
      >
        {([k, v]) => (
          <div
            style={{
              display: 'flex',
              'justify-content': 'space-between',
              padding: '8px 0',
              'border-top': '1px solid var(--safai-line)',
              'font-size': '12px',
            }}
          >
            <span style={{ color: 'var(--safai-fg-2)' }}>{k}</span>
            <span class="num" style={{ color: 'var(--safai-fg-0)' }}>
              {v}
            </span>
          </div>
        )}
      </For>
    </div>
  );
}

function sudsCopy(cat: JunkCategoryReport | undefined, report: JunkReport | undefined): string {
  if (cat) return cat.description;
  if (report && report.totalBytes > 0) {
    return `Rummaged around and turned up ${formatBytes(
      report.totalBytes,
    )} of clutter across ${formatCount(report.totalItems)} items. Tap a row to peek inside.`;
  }
  return 'Nothing obvious to sweep here. Your caches are tidy - nice.';
}

function SkeletonRow() {
  return (
    <div
      class="safai-card"
      style={{
        padding: '14px 16px',
        height: '54px',
        'margin-bottom': '8px',
        background:
          'linear-gradient(90deg, var(--safai-bg-2) 0%, var(--safai-bg-3) 50%, var(--safai-bg-2) 100%)',
        'background-size': '200% 100%',
        animation: 'safai-shimmer 1.4s ease-in-out infinite',
      }}
    />
  );
}

function ErrorCard(props: { message: string; onRetry: () => void }) {
  return (
    <div
      class="safai-card"
      style={{
        padding: '24px 28px',
        display: 'flex',
        'align-items': 'center',
        gap: '20px',
        'margin-bottom': '20px',
        border: '1px solid oklch(0.68 0.18 25 / 0.4)',
      }}
    >
      <Suds size={56} mood="shocked" />
      <div style={{ flex: 1 }}>
        <div style={{ 'font-size': '14px', color: 'var(--safai-fg-0)', 'margin-bottom': '4px' }}>
          Couldn't scan junk
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
