import {
  createEffect,
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
} from 'solid-js';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon, type IconName } from '../components/Icon';
import { ConfirmDeleteModal } from '../components/ConfirmDeleteModal';
import { CacheFreshness } from '../components/CacheFreshness';
import {
  privacyScan,
  selectedPathsFor,
  type BrowserReport,
  type PrivacyCategoryReport,
  type PrivacyReport,
} from '../lib/privacy';
import { CACHE_PRIVACY, KEY_PRIVACY, sharedResource } from '../lib/scanCache';
import {
  commitDelete,
  previewDelete,
  restoreLast,
  type DeletePlan,
  type DeleteResult,
} from '../lib/cleaner';
import { invalidateFilesystemCachesSoon } from '../lib/cacheInvalidation';
import { formatBytes, formatCount, formatRelativeTime } from '../lib/format';

// privacy cleaner. one card per installed browser, expands to a per-category
// checkbox grid. selection key is "<browser-id>::<category-id>" so clearing
// chrome history but keeping firefox's works. cleans via the shared cleaner
// (preview -> confirm -> commit), restore last same as Junk.
export default function Privacy() {
  const [report, { refetch }] = sharedResource(CACHE_PRIVACY, privacyScan);

  // drives "last scanned Xs ago" + the feedback latch below. 250ms is
  // snappy enough for a visible release
  const [clock, setClock] = createSignal(Date.now());
  onMount(() => {
    const tick = window.setInterval(() => setClock(Date.now()), 250);
    onCleanup(() => window.clearInterval(tick));
  });

  // min-visible scan state so a sub-100ms rescan still registers (same
  // trick as Junk)
  const MIN_FEEDBACK_MS = 900;
  const [rescanAt, setRescanAt] = createSignal<number | null>(null);
  const rescanning = () => {
    const t = rescanAt();
    return t != null && clock() - t < MIN_FEEDBACK_MS;
  };
  const doRescan = () => {
    setRescanAt(Date.now());
    refetch();
  };
  const isBusy = () => report.loading || rescanning();

  // default: everything except history + cookies, those are invasive
  // enough that they should be opt-in not opt-out
  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [expanded, setExpanded] = createSignal<Set<string>>(new Set());
  let lastScanStamp: number | null = null;
  createEffect(() => {
    const r = report();
    if (!r) return;
    if (r.scannedAt === lastScanStamp) return;
    lastScanStamp = r.scannedAt;
    const s = new Set<string>();
    const e = new Set<string>();
    for (const b of r.browsers) {
      if (!b.available) continue;
      e.add(b.id);
      for (const c of b.categories) {
        if (c.bytes === 0) continue;
        // safe defaults: cache + sessions + local-storage. cookies +
        // history log the user out / wipe their trail, opt-in only
        if (c.id === 'cache' || c.id === 'sessions' || c.id === 'local-storage') {
          s.add(`${b.id}::${c.id}`);
        }
      }
    }
    setSelected(s);
    setExpanded(e);
  });

  const toggleSelected = (key: string) => {
    const s = new Set(selected());
    if (s.has(key)) s.delete(key);
    else s.add(key);
    setSelected(s);
  };

  const toggleExpanded = (id: string) => {
    const s = new Set(expanded());
    if (s.has(id)) s.delete(id);
    else s.add(id);
    setExpanded(s);
  };

  const selectedBytes = createMemo(() => {
    const r = report();
    if (!r) return 0;
    const s = selected();
    let out = 0;
    for (const b of r.browsers) {
      for (const c of b.categories) {
        if (s.has(`${b.id}::${c.id}`)) out += c.bytes;
      }
    }
    return out;
  });

  const pathsToDelete = createMemo<string[]>(() => {
    const r = report();
    if (!r) return [];
    return selectedPathsFor(r, selected());
  });

  // delete flow, same state machine as Junk
  const [pendingPlan, setPendingPlan] = createSignal<DeletePlan | null>(null);
  const [planError, setPlanError] = createSignal<string | null>(null);
  const [committing, setCommitting] = createSignal(false);
  const [commitResult, setCommitResult] = createSignal<DeleteResult | null>(null);
  const [restoring, setRestoring] = createSignal(false);
  const [restoreMsg, setRestoreMsg] = createSignal<string | null>(null);

  const openConfirm = async () => {
    setPlanError(null);
    setCommitResult(null);
    const paths = pathsToDelete();
    if (paths.length === 0) return;
    try {
      setPendingPlan(await previewDelete(paths));
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
      invalidateFilesystemCachesSoon(KEY_PRIVACY);
      refetch();
    } catch (e) {
      setPlanError(String(e));
    } finally {
      setCommitting(false);
    }
  };

  const doRestore = async () => {
    if (restoring()) return;
    setRestoring(true);
    setRestoreMsg(null);
    try {
      const r = await restoreLast();
      const n = r.restored.length;
      setRestoreMsg(
        n === 0
          ? 'Nothing to restore.'
          : `Restored ${formatCount(n)} item${n === 1 ? '' : 's'} (${formatBytes(r.bytesRestored)}).`,
      );
      invalidateFilesystemCachesSoon(KEY_PRIVACY);
      refetch();
    } catch (e) {
      setRestoreMsg(`Couldn't restore: ${String(e)}`);
    } finally {
      setRestoring(false);
    }
  };

  const availableBrowsers = () => report()?.browsers.filter((b) => b.available) ?? [];
  const totalAvailableBytes = () =>
    availableBrowsers().reduce((acc, b) => acc + b.bytes, 0);

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Cleanup"
        title="Privacy"
        subtitle="Clear browser caches, cookies, history, and stored sessions."
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
                pathsToDelete().length === 0
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
            <SummaryBar
              totalAvailable={totalAvailableBytes()}
              selected={selectedBytes()}
              loading={report.loading && !report()}
            />

            <Show
              when={!report.loading || report()}
              fallback={<For each={Array.from({ length: 4 })}>{() => <SkeletonRow />}</For>}
            >
              <For each={report()?.browsers ?? []}>
                {(browser) => (
                  <BrowserCard
                    browser={browser}
                    expanded={expanded().has(browser.id)}
                    selectedSet={selected()}
                    onToggleExpanded={() => toggleExpanded(browser.id)}
                    onToggleCategory={(catId) =>
                      toggleSelected(`${browser.id}::${catId}`)
                    }
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
                    'flex-wrap': 'wrap',
                  }}
                  aria-live="polite"
                >
                  <Show
                    when={isBusy()}
                    fallback={<span>Last scanned {formatRelativeTime(r().scannedAt, clock())}</span>}
                  >
                    <span style={{ color: 'var(--safai-cyan)' }}>
                      <span
                        class="safai-spin"
                        style={{ display: 'inline-block', 'margin-right': '6px' }}
                      >
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
                  <span>{availableBrowsers().length} browser{availableBrowsers().length === 1 ? '' : 's'} found</span>
                  <span>·</span>
                  <span>Platform: {r().platform}</span>
                  <span>·</span>
                  <CacheFreshness
                    cacheKey={CACHE_PRIVACY}
                    version={r()}
                    disabled={isBusy()}
                    onRescan={doRescan}
                  />
                </div>
              )}
            </Show>
          </Show>
        </div>

        <SidePane report={report()} selectedBytes={selectedBytes()} />
      </div>

      <Show when={planError() || commitResult() || restoreMsg()}>
        <StatusBanner
          error={planError()}
          commit={commitResult()}
          restore={restoreMsg()}
          onDismiss={() => {
            setPlanError(null);
            setCommitResult(null);
            setRestoreMsg(null);
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

// subcomponents

function SummaryBar(props: { totalAvailable: number; selected: number; loading: boolean }) {
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
      <Icon name="shield" size={14} color="var(--safai-cyan)" />
      <span style={{ 'font-size': '12px', color: 'var(--safai-fg-1)' }}>
        Across all installed browsers:{' '}
        <span class="num">{props.loading ? '-' : formatBytes(props.selected)}</span>{' '}
        selected of <span class="num">{formatBytes(props.totalAvailable)}</span> available
      </span>
    </div>
  );
}

function BrowserCard(props: {
  browser: BrowserReport;
  expanded: boolean;
  selectedSet: Set<string>;
  onToggleExpanded: () => void;
  onToggleCategory: (catId: PrivacyCategoryReport['id']) => void;
}) {
  const categoryKey = (c: PrivacyCategoryReport) => `${props.browser.id}::${c.id}`;
  const selectedCount = () =>
    props.browser.categories.filter((c) => props.selectedSet.has(categoryKey(c))).length;

  return (
    <div class="safai-card" style={{ 'margin-bottom': '8px', overflow: 'hidden' }}>
      <div
        style={{
          padding: '14px 16px',
          display: 'flex',
          'align-items': 'center',
          gap: '12px',
          cursor: props.browser.available ? 'pointer' : 'default',
          opacity: props.browser.available ? 1 : 0.5,
        }}
        onClick={() => props.browser.available && props.onToggleExpanded()}
      >
        <Icon
          name={props.expanded ? 'chevronD' : 'chevronR'}
          size={12}
          color="var(--safai-fg-2)"
        />
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
          <Icon name={props.browser.icon as IconName} size={14} color="var(--safai-cyan)" />
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
            {props.browser.label}
            <Show when={!props.browser.available}>
              <span
                class="safai-pill"
                style={{
                  background: 'var(--safai-bg-2)',
                  color: 'var(--safai-fg-3)',
                  'font-size': '9px',
                }}
              >
                not installed
              </span>
            </Show>
            <Show when={props.browser.available && props.browser.profiles.length > 1}>
              <span
                class="safai-pill"
                style={{
                  background: 'var(--safai-cyan-dim)',
                  color: 'var(--safai-cyan)',
                  'font-size': '9px',
                }}
              >
                {props.browser.profiles.length} profiles
              </span>
            </Show>
          </div>
          <div
            style={{
              'font-size': '11px',
              color: 'var(--safai-fg-3)',
              'margin-top': '2px',
              'white-space': 'nowrap',
              overflow: 'hidden',
              'text-overflow': 'ellipsis',
            }}
            title={props.browser.root}
          >
            <Show
              when={props.browser.available}
              fallback={<>Nothing from {props.browser.label} on this system.</>}
            >
              {formatCount(props.browser.items)} items ·{' '}
              {selectedCount() === 0
                ? 'no categories selected'
                : `${selectedCount()} of ${props.browser.categories.length} categories selected`}
            </Show>
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
          {formatBytes(props.browser.bytes)}
        </div>
      </div>
      <Show when={props.expanded && props.browser.available}>
        <div
          style={{
            'border-top': '1px solid var(--safai-line)',
            background: 'oklch(0.18 0.008 240)',
          }}
        >
          <For each={props.browser.categories}>
            {(cat) => (
              <CategoryRow
                cat={cat}
                selected={props.selectedSet.has(categoryKey(cat))}
                onToggle={() => props.onToggleCategory(cat.id)}
              />
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}

function CategoryRow(props: {
  cat: PrivacyCategoryReport;
  selected: boolean;
  onToggle: () => void;
}) {
  const disabled = () => props.cat.bytes === 0;
  return (
    <div
      style={{
        padding: '10px 16px 10px 44px',
        display: 'flex',
        'align-items': 'center',
        gap: '12px',
        'border-bottom': '1px solid var(--safai-line)',
        cursor: disabled() ? 'default' : 'pointer',
        opacity: disabled() ? 0.45 : 1,
      }}
      onClick={() => !disabled() && props.onToggle()}
    >
      <div
        class={`safai-check safai-check--${props.selected && !disabled() ? 'on' : 'off'}`}
        aria-label={`Toggle ${props.cat.label}`}
      >
        <Show when={props.selected && !disabled()}>
          <Icon name="check" size={9} color="oklch(0.18 0.02 240)" strokeWidth={2.2} />
        </Show>
      </div>
      <div
        style={{
          width: '22px',
          height: '22px',
          'border-radius': '6px',
          background: 'var(--safai-bg-3)',
          display: 'flex',
          'align-items': 'center',
          'justify-content': 'center',
        }}
      >
        <Icon name={props.cat.icon as IconName} size={11} color="var(--safai-fg-2)" />
      </div>
      <div style={{ flex: 1, 'min-width': 0 }}>
        <div style={{ 'font-size': '12px', 'font-weight': 500, color: 'var(--safai-fg-1)' }}>
          {props.cat.label}
        </div>
        <div style={{ 'font-size': '11px', color: 'var(--safai-fg-3)', 'margin-top': '1px' }}>
          {props.cat.description}
          <Show when={props.cat.targets.length > 0}>
            {' · '}
            {formatCount(props.cat.items)} files across {props.cat.targets.length} target
            {props.cat.targets.length === 1 ? '' : 's'}
          </Show>
        </div>
      </div>
      <div class="num" style={{ 'font-size': '12px', color: 'var(--safai-fg-1)' }}>
        {formatBytes(props.cat.bytes)}
      </div>
    </div>
  );
}

function SidePane(props: { report: PrivacyReport | undefined; selectedBytes: number }) {
  const available = () => props.report?.browsers.filter((b) => b.available) ?? [];
  const bucketNote = () => {
    const n = available().length;
    if (n === 0) return 'No installed browsers detected - install Chrome, Edge, Firefox, or Safari to get going.';
    if (props.selectedBytes === 0) {
      return `Found ${n} browser${n === 1 ? '' : 's'}. Tick categories on the left; safe choices (caches, sessions, local storage) are pre-selected.`;
    }
    return `Cleaning moves ${formatBytes(props.selectedBytes)} to Safai's graveyard. Restore last puts it right back.`;
  };

  return (
    <div>
      <div class="safai-card" style={{ padding: '18px', 'margin-bottom': '12px' }}>
        <div
          style={{
            display: 'flex',
            'align-items': 'center',
            gap: '10px',
            'margin-bottom': '12px',
          }}
        >
          <Suds size={36} mood="happy" />
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
          {bucketNote()}
        </div>
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-3)',
            'padding-top': '12px',
            'border-top': '1px solid var(--safai-line)',
          }}
        >
          Clearing cookies signs you out of sites. Clearing history loses your trail. Cache +
          sessions + local storage are safe defaults.
        </div>
      </div>

      <Show when={available().length > 0}>
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
            Installed browsers
          </div>
          <For each={available()}>
            {(b) => (
              <div
                style={{
                  display: 'flex',
                  'justify-content': 'space-between',
                  padding: '8px 0',
                  'border-top': '1px solid var(--safai-line)',
                  'font-size': '12px',
                }}
              >
                <span style={{ color: 'var(--safai-fg-2)' }}>{b.label}</span>
                <span class="num" style={{ color: 'var(--safai-fg-0)' }}>
                  {formatBytes(b.bytes)}
                </span>
              </div>
            )}
          </For>
        </div>
      </Show>
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
          Couldn't scan privacy data
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
