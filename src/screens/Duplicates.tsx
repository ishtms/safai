import {
  createEffect,
  createMemo,
  createResource,
  createSignal,
  For,
  onCleanup,
  Show,
} from 'solid-js';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon } from '../components/Icon';
import { ConfirmDeleteModal } from '../components/ConfirmDeleteModal';
import {
  autoKeepOriginal,
  cancelDuplicates,
  forgetDuplicates,
  phaseLabel,
  startDuplicates,
  subscribeDuplicates,
  type DuplicateGroup,
  type DuplicateReport,
} from '../lib/duplicates';
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
  formatRelativeTime,
  splitBytes,
} from '../lib/format';

// duplicate finder. paginates because real home dirs hit thousands of
// groups (node_modules mirrors, browser cache dupes, photo thumbs) and 6k+
// cards at once stalls the main thread. top N by wasted bytes, expand on
// demand. selection ops always run on the full group set so Clean selected
// covers hidden groups too.
const VISIBLE_GROUPS_STEP = 50;
// hard cap on files inside one card. node_modules can hit 100+ copies,
// "show all N" expansion keeps the card scannable
const FILES_PER_GROUP_PREVIEW = 8;

export default function Duplicates() {
  const [response, setResponse] = createSignal<DuplicateReport | null>(null);
  const [scanning, setScanning] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  // per-group: path to KEEP, everything else in that group is a delete
  // candidate. storing "kept" (one) instead of "marked" (N) makes it
  // impossible to accidentally mark every file for deletion
  const [kept, setKept] = createSignal<Record<string, string>>({});
  const [disabled, setDisabled] = createSignal<Set<string>>(new Set<string>());
  const [visibleCount, setVisibleCount] = createSignal(VISIBLE_GROUPS_STEP);

  let currentHandle: string | null = null;
  let unsubscribe: (() => void) | null = null;

  const stopWalk = async () => {
    if (currentHandle) {
      try {
        await cancelDuplicates(currentHandle);
        await forgetDuplicates(currentHandle);
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
    setResponse(null);
    setDisabled(new Set<string>());
    setVisibleCount(VISIBLE_GROUPS_STEP);
    try {
      unsubscribe = await subscribeDuplicates({
        onProgress: (r) => setResponse(r),
        onDone: (r) => {
          setResponse(r);
          setScanning(false);
        },
      });
      const handle = await startDuplicates({});
      currentHandle = handle.id;
    } catch (e) {
      setError(String(e));
      setScanning(false);
    }
  };

  void startWalk();
  onCleanup(() => {
    void stopWalk();
  });

  // seed selection once we have actual groups. progress events carry
  // groups: [] so the non-empty guard works without needing the phase
  // field (old rust binaries without phase plumbing still work)
  let lastSeededReportKey: string | null = null;
  createEffect(() => {
    const r = response();
    if (!r || r.groups.length === 0) return;
    const key = `${r.root}/${r.totalGroups}/${r.durationMs}`;
    if (key === lastSeededReportKey) return;
    lastSeededReportKey = key;
    const newKept: Record<string, string> = {};
    for (const g of r.groups) {
      newKept[g.id] = autoKeepOriginal(g);
    }
    setKept(newKept);
    setDisabled(new Set<string>());
  });

  const setKeptFor = (groupId: string, path: string) => {
    setKept((k) => ({ ...k, [groupId]: path }));
  };
  const toggleEnabled = (groupId: string) => {
    const next = new Set(disabled());
    if (next.has(groupId)) next.delete(groupId);
    else next.add(groupId);
    setDisabled(next);
  };

  // unconditional read is safe, progress events deliver groups: []. works
  // even without the phase field (stale tauri dev builds)
  const allGroups = () => response()?.groups ?? [];

  const visibleGroups = createMemo<DuplicateGroup[]>(() => {
    const gs = allGroups();
    const n = visibleCount();
    return n >= gs.length ? gs : gs.slice(0, n);
  });

  const selectedPaths = createMemo<string[]>(() => {
    const gs = allGroups();
    if (gs.length === 0) return [];
    const k = kept();
    const d = disabled();
    const out: string[] = [];
    for (const g of gs) {
      if (d.has(g.id)) continue;
      const keepPath = k[g.id] ?? g.files[0]?.path;
      if (!keepPath) continue;
      for (const f of g.files) {
        if (f.path !== keepPath) out.push(f.path);
      }
    }
    return out;
  });

  const selectedBytes = createMemo(() => {
    const gs = allGroups();
    if (gs.length === 0) return 0;
    const d = disabled();
    let total = 0;
    for (const g of gs) if (!d.has(g.id)) total += g.wastedBytes;
    return total;
  });

  // delete flow, same as Junk.tsx since both route through the cleaner
  const [pendingPlan, setPendingPlan] = createSignal<DeletePlan | null>(null);
  const [planError, setPlanError] = createSignal<string | null>(null);
  const [committing, setCommitting] = createSignal(false);
  const [commitResult, setCommitResult] = createSignal<DeleteResult | null>(null);
  const [restoring, setRestoring] = createSignal(false);
  const [restoreMessage, setRestoreMessage] = createSignal<string | null>(null);

  const [stats, { refetch: refetchStats }] = createResource<GraveyardStats>(graveyardStats);

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
      // rescan so freshly-emptied groups drop out
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

  const hasGroups = () => allGroups().length > 0;
  const hiddenGroupCount = () => {
    const n = allGroups().length - visibleCount();
    return n > 0 ? n : 0;
  };
  const hiddenRecoverable = () => {
    const gs = allGroups();
    const start = Math.min(visibleCount(), gs.length);
    let t = 0;
    for (let i = start; i < gs.length; i++) t += gs[i].wastedBytes;
    return t;
  };

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Cleanup"
        title="Duplicates"
        subtitle="Byte-identical files taking up space more than once."
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
              disabled={
                scanning() ||
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

        <Show when={!error()}>
          <Show
            when={hasGroups()}
            fallback={
              <Show when={scanning()} fallback={<EmptyState />}>
                <ScanningCard response={response()} />
              </Show>
            }
          >
            <For each={visibleGroups()}>
              {(group) => (
                <GroupCard
                  group={group}
                  keptPath={kept()[group.id] ?? group.files[0]?.path ?? ''}
                  enabled={!disabled().has(group.id)}
                  onSelectKept={(path) => setKeptFor(group.id, path)}
                  onToggleEnabled={() => toggleEnabled(group.id)}
                />
              )}
            </For>
            <Show when={hiddenGroupCount() > 0}>
              <button
                class="safai-card safai-card--hover"
                onClick={() =>
                  setVisibleCount((n) =>
                    Math.min(n + VISIBLE_GROUPS_STEP, allGroups().length),
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
                  {Math.min(VISIBLE_GROUPS_STEP, hiddenGroupCount())} of{' '}
                  <span class="num">{formatCount(hiddenGroupCount())}</span> hidden groups
                </span>
                <span
                  style={{
                    'margin-left': 'auto',
                    'font-size': '11px',
                    color: 'var(--safai-fg-3)',
                  }}
                >
                  <span class="num">{formatBytes(hiddenRecoverable())}</span> more recoverable
                </span>
              </button>
            </Show>
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
                <span>Scanned {formatCount(r().totalFilesScanned)} files</span>
                <span>·</span>
                <span>{formatCount(r().totalGroups)} duplicate groups</span>
                <Show when={allGroups().length > visibleCount()}>
                  <span>·</span>
                  <span>
                    showing{' '}
                    <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
                      {formatCount(Math.min(visibleCount(), allGroups().length))}
                    </span>
                  </span>
                </Show>
                <span>·</span>
                <span>{Math.max(1, Math.round(r().durationMs))} ms</span>
                <span>·</span>
                <span>≥ 1 MB files only</span>
                <span>·</span>
                <span
                  title="Package-manager + build-tool directories are skipped so installer-managed files don't look like duplicates."
                  style={{ 'border-bottom': '1px dotted var(--safai-fg-3)', cursor: 'help' }}
                >
                  skipping node_modules, .git, Pods, DerivedData, __pycache__, venv, vendor, .gradle, target, Cellar, .pnpm-store, .yarn
                </span>
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

function HeroStrip(props: { response: DuplicateReport | null; scanning: boolean }) {
  const wasted = () => props.response?.wastedBytes ?? 0;
  const groups = () => props.response?.totalGroups ?? 0;
  const split = () => splitBytes(wasted());
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
      <Suds size={64} mood={props.scanning ? 'happy' : groups() > 0 ? 'shocked' : 'happy'} float={props.scanning} />
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
          Recoverable
        </div>
        <div
          style={{
            display: 'flex',
            'align-items': 'baseline',
            gap: '10px',
          }}
        >
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
            across {formatCount(groups())} duplicate group{groups() === 1 ? '' : 's'}
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
      Hashing
    </div>
  );
}

// group card

function GroupCard(props: {
  group: DuplicateGroup;
  keptPath: string;
  enabled: boolean;
  onSelectKept: (path: string) => void;
  onToggleEnabled: () => void;
}) {
  const deleteCount = () => props.group.files.length - 1;
  const now = Date.now();
  const [expandFiles, setExpandFiles] = createSignal(false);
  const visibleFiles = createMemo(() => {
    const all = props.group.files;
    if (expandFiles() || all.length <= FILES_PER_GROUP_PREVIEW) return all;
    // always include the kept path so the radio stays visible regardless
    // of its position in sort order
    const preview = all.slice(0, FILES_PER_GROUP_PREVIEW);
    if (preview.some((f) => f.path === props.keptPath)) return preview;
    const kept = all.find((f) => f.path === props.keptPath);
    return kept ? [kept, ...preview.slice(0, FILES_PER_GROUP_PREVIEW - 1)] : preview;
  });
  const hiddenFileCount = () => props.group.files.length - visibleFiles().length;
  return (
    <div
      class="safai-card"
      style={{
        overflow: 'hidden',
        // parent is flex column, without this 50+ cards get crushed to
        // ~20px each by default flex-shrink: 1 and the card looks empty.
        // shrink: 0 keeps natural height, parent's overflow: auto handles
        // the excess
        'flex-shrink': 0,
      }}
    >
      <div
        style={{
          padding: '14px 18px',
          display: 'flex',
          'align-items': 'center',
          gap: '14px',
          'border-bottom': '1px solid var(--safai-line)',
          background: 'oklch(0.18 0.008 240)',
        }}
      >
        <div
          class={`safai-check safai-check--${props.enabled ? 'on' : 'off'}`}
          onClick={props.onToggleEnabled}
          aria-label={props.enabled ? 'Disable this group' : 'Enable this group'}
          role="checkbox"
          aria-checked={props.enabled}
          tabindex={0}
          onKeyDown={(e) => {
            if (e.key === ' ' || e.key === 'Enter') {
              e.preventDefault();
              props.onToggleEnabled();
            }
          }}
          style={{ cursor: 'pointer' }}
        >
          <Show when={props.enabled}>
            <Icon name="check" size={9} color="oklch(0.18 0.02 240)" strokeWidth={2.2} />
          </Show>
        </div>
        <div
          style={{
            width: '32px',
            height: '32px',
            'border-radius': '8px',
            background: 'color-mix(in oklab, var(--safai-cyan) 12%, transparent)',
            display: 'flex',
            'align-items': 'center',
            'justify-content': 'center',
          }}
        >
          <Icon name="copy" size={14} color="var(--safai-cyan)" />
        </div>
        <div style={{ flex: 1, 'min-width': 0 }}>
          <div style={{ 'font-size': '13px', 'font-weight': 500 }}>
            {formatCount(props.group.files.length)} identical files ·{' '}
            <span class="num">{formatBytes(props.group.bytesEach)}</span> each
          </div>
          <div
            class="mono"
            style={{
              'font-size': '10px',
              color: 'var(--safai-fg-3)',
              'margin-top': '2px',
              'font-variant-numeric': 'tabular-nums',
            }}
          >
            {props.group.id}
          </div>
        </div>
        <div style={{ 'text-align': 'right' }}>
          <div
            class="num"
            style={{
              'font-size': '15px',
              'font-weight': 600,
              'font-family': 'var(--safai-font-display)',
              'font-variant-numeric': 'tabular-nums',
              color: 'var(--safai-cyan)',
            }}
          >
            {formatBytes(props.group.wastedBytes)}
          </div>
          <div style={{ 'font-size': '10px', color: 'var(--safai-fg-3)' }}>
            recoverable · delete {formatCount(deleteCount())}
          </div>
        </div>
      </div>

      <div>
        <For each={visibleFiles()}>
          {(file) => {
            const isKept = () => file.path === props.keptPath;
            return (
              <div
                style={{
                  display: 'flex',
                  'align-items': 'center',
                  gap: '12px',
                  padding: '10px 18px',
                  'border-bottom': '1px solid var(--safai-line)',
                  cursor: 'pointer',
                  background: isKept()
                    ? 'color-mix(in oklab, var(--safai-cyan) 8%, transparent)'
                    : 'transparent',
                }}
                onClick={() => props.onSelectKept(file.path)}
              >
                <div
                  role="radio"
                  aria-checked={isKept()}
                  tabindex={0}
                  style={{
                    width: '16px',
                    height: '16px',
                    'border-radius': '50%',
                    border: '1.5px solid',
                    'border-color': isKept() ? 'var(--safai-cyan)' : 'var(--safai-line)',
                    display: 'flex',
                    'align-items': 'center',
                    'justify-content': 'center',
                    'flex-shrink': 0,
                  }}
                  onKeyDown={(e) => {
                    if (e.key === ' ' || e.key === 'Enter') {
                      e.preventDefault();
                      props.onSelectKept(file.path);
                    }
                  }}
                >
                  <Show when={isKept()}>
                    <span
                      style={{
                        width: '8px',
                        height: '8px',
                        'border-radius': '50%',
                        background: 'var(--safai-cyan)',
                      }}
                    />
                  </Show>
                </div>
                <div style={{ flex: 1, 'min-width': 0 }}>
                  <div
                    class="mono"
                    style={{
                      'font-size': '12px',
                      color: isKept() ? 'var(--safai-fg-0)' : 'var(--safai-fg-1)',
                      'white-space': 'nowrap',
                      overflow: 'hidden',
                      'text-overflow': 'ellipsis',
                    }}
                    title={file.path}
                  >
                    {file.path}
                  </div>
                  <div
                    style={{
                      'font-size': '10px',
                      color: 'var(--safai-fg-3)',
                      'margin-top': '2px',
                    }}
                  >
                    <Show
                      when={file.modified != null}
                      fallback={<span>Unknown mtime</span>}
                    >
                      Modified {formatRelativeTime(file.modified, now)}
                    </Show>
                  </div>
                </div>
                <div
                  class="safai-pill"
                  style={{
                    'font-size': '9px',
                    background: isKept()
                      ? 'color-mix(in oklab, var(--safai-cyan) 22%, transparent)'
                      : 'var(--safai-bg-2)',
                    color: isKept() ? 'var(--safai-cyan)' : 'var(--safai-fg-3)',
                    'letter-spacing': '0.08em',
                    'text-transform': 'uppercase',
                  }}
                >
                  {isKept() ? 'Keep' : 'Delete'}
                </div>
              </div>
            );
          }}
        </For>
        <Show when={hiddenFileCount() > 0}>
          <button
            class="safai-btn safai-btn--ghost"
            style={{
              width: '100%',
              padding: '10px 18px',
              'justify-content': 'center',
              'font-size': '11px',
              'border-radius': 0,
            }}
            onClick={() => setExpandFiles(true)}
          >
            <Icon name="chevronD" size={11} /> Show {formatCount(hiddenFileCount())} more cop
            {hiddenFileCount() === 1 ? 'y' : 'ies'}
          </button>
        </Show>
      </div>
    </div>
  );
}

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
          No duplicates found.
        </div>
        <div style={{ 'font-size': '12px', color: 'var(--safai-fg-2)', 'line-height': 1.5 }}>
          Suds walked your home directory, grouped by size, then by partial
          hash, then by full blake3 — and didn't find any byte-identical
          pairs worth cleaning up. Nice.
        </div>
      </div>
    </div>
  );
}

function SkeletonGroup() {
  return (
    <div
      class="safai-card"
      style={{
        height: '180px',
        'flex-shrink': 0,
        background:
          'linear-gradient(90deg, var(--safai-bg-2) 0%, var(--safai-bg-3) 50%, var(--safai-bg-2) 100%)',
        'background-size': '200% 100%',
        animation: 'safai-shimmer 1.4s ease-in-out infinite',
      }}
    />
  );
}

function ScanningCard(props: { response: DuplicateReport | null }) {
  const phase = () => props.response?.phase;
  const label = () => (phase() ? phaseLabel(phase()!) : 'Starting scan');
  const files = () => props.response?.totalFilesScanned ?? 0;
  const remaining = () => props.response?.candidatesRemaining ?? 0;
  return (
    <div>
      <div
        class="safai-card"
        style={{
          padding: '22px 26px',
          display: 'flex',
          'align-items': 'center',
          gap: '18px',
          'margin-bottom': '10px',
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
            <Show when={phase() === 'size-grouped' || phase() === 'head-hashed'}>
              {' · '}
              <span class="num" style={{ color: 'var(--safai-fg-0)' }}>
                {formatCount(remaining())}
              </span>{' '}
              candidates remaining
            </Show>
          </div>
        </div>
      </div>
      <For each={Array.from({ length: 3 })}>{() => <SkeletonGroup />}</For>
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
          Couldn't run the duplicate hunt
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
      const base = `Cleaned ${formatCount(n)} duplicate${n === 1 ? '' : 's'} · ${formatBytes(
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
