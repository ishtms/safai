import { createMemo, createResource, createSignal, For, onCleanup, onMount, Show } from 'solid-js';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon } from '../components/Icon';
import {
  invalidateTreemapCache,
  cancelTreemap,
  forgetTreemap,
  serveTreemapSubtree,
  startTreemap,
  subscribeTreemap,
  tileColor,
  treemapSnapshot,
  type BiggestFolder,
  type TreemapResponse,
  type TreemapTile,
} from '../lib/treemap';
import { listVolumes, pickPrimary, type Volume } from '../lib/volumes';
import { formatBytes, formatCount, formatRelativeTime, splitBytes } from '../lib/format';
import {
  fetchPermissionStatus,
  openPermissionSettings,
  type PermissionStatusEntry,
} from '../lib/onboarding';

// disk usage treemap.
// streams treemap://progress every ~150ms, terminal treemap://done. ui swaps in
// the latest. first snapshot lands after the first dir batch so 250k files is
// usable in a few hundred ms.
// rects are absolutely-positioned divs w/ % coords from rust. labels as html
// so they don't stretch like svg text in a unit-square viewBox does.
export default function Disk() {
  // empty stack = $HOME, backend resolves it
  const [stack, setStack] = createSignal<string[]>([]);
  const currentRoot = () => stack()[stack().length - 1];

  // populated from progress + done events. no blocking resource, we want
  // partial tiles that grow, not "0 B" until done
  const [response, setResponse] = createSignal<TreemapResponse | null>(null);
  const [scanning, setScanning] = createSignal<boolean>(false);
  const [error, setError] = createSignal<string | null>(null);

  const [volumes] = createResource(listVolumes);
  const primary = () => (volumes() ? pickPrimary(volumes()!) : null);
  // only used for the mac FDA CTA; non-mac returns an empty array or
  // entries with status unknown and we hide the CTA anyway
  const [permStatus] = createResource(fetchPermissionStatus);

  let unsubscribe: (() => void) | null = null;
  let activeHandleId: string | null = null;
  let disposed = false;

  const finishWalk = (id: string, r: TreemapResponse) => {
    if (activeHandleId !== id) return;
    activeHandleId = null;
    setResponse(r);
    setScanning(false);
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
    void forgetTreemap(id);
  };

  const startWalk = async (root: string | undefined) => {
    if (disposed) return;
    await detachWalk(true);
    if (disposed) return;
    setError(null);
    setScanning(true);

    try {
      const handle = await startTreemap({ root, depth: 4 });
      if (disposed) {
        await cancelTreemap(handle.id).catch(() => {});
        await forgetTreemap(handle.id).catch(() => {});
        return;
      }
      activeHandleId = handle.id;
      const nextUnsubscribe = await subscribeTreemap(handle.id, {
        onProgress: (r) => {
          if (disposed) return;
          if (activeHandleId === handle.id) setResponse(r);
        },
        onDone: (r) => finishWalk(handle.id, r),
      });
      if (disposed || activeHandleId !== handle.id) {
        nextUnsubscribe();
        if (disposed) {
          activeHandleId = null;
          await cancelTreemap(handle.id).catch(() => {});
          await forgetTreemap(handle.id).catch(() => {});
        }
        return;
      }
      unsubscribe = nextUnsubscribe;
      const terminal = await treemapSnapshot(handle.id);
      if (!disposed && terminal) finishWalk(handle.id, terminal);
    } catch (e) {
      if (disposed) return;
      setError(String(e));
      setScanning(false);
    }
  };

  const detachWalk = async (cancelActive: boolean = false): Promise<void> => {
    if (unsubscribe) {
      unsubscribe();
      unsubscribe = null;
    }
    if (cancelActive && activeHandleId) {
      const id = activeHandleId;
      activeHandleId = null;
      await cancelTreemap(id).catch(() => {});
      await forgetTreemap(id).catch(() => {});
    }
  };

  // try cache first, fall back to a streaming walk. the rust side seeds
  // the cache on done so drill/back hits next time. this is what fixes
  // "it rescans when i go back to Home"
  const navigateTo = async (root: string | undefined) => {
    if (disposed) return;
    try {
      const cached = await serveTreemapSubtree(root);
      if (disposed) return;
      if (cached) {
        // kill any in-flight walk, its emit_done would clobber the cached
        // response with older/wider state
        await detachWalk(true);
        setError(null);
        setResponse(cached);
        setScanning(false);
        return;
      }
    } catch {
      // cache miss (tauri offline, bad path), fall through to real walk
    }
    await startWalk(root);
  };

  // first mount misses by definition, later mounts hit
  onMount(() => {
    void navigateTo(undefined);
  });

  onCleanup(() => {
    disposed = true;
    void detachWalk(true);
  });

  const drillInto = async (tile: TreemapTile) => {
    if (!tile.isDir || tile.isOther) return;
    // no drilling mid-scan. interrupts the walk + users lose progress
    // when they come back. wait for it to finish
    if (scanning()) return;
    setStack((s) => [...s, tile.path]);
    setResponse(null);
    await navigateTo(tile.path);
  };

  const popTo = async (idx: number) => {
    if (scanning()) return;
    setStack((s) => s.slice(0, idx + 1));
    setResponse(null);
    await navigateTo(currentRoot());
  };

  const goHome = async () => {
    if (scanning()) return;
    setStack([]);
    setResponse(null);
    await navigateTo(undefined);
  };

  // user-clicked rescan. nuke rust cache first so the fresh walk isn't
  // short-circuited, then restart from current root
  const rescan = async () => {
    if (scanning()) return;
    try {
      await invalidateTreemapCache();
    } catch {
      // worst case rescan returns cached data, user can drill to verify
    }
    setStack([]);
    setResponse(null);
    await startWalk(undefined);
  };

  const canGoBack = () => stack().length > 0;
  const resp = () => response();

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Overview"
        title="Disk Usage"
        subtitle="Find out what's really taking up space - every rectangle is a folder."
        right={
          <div style={{ display: 'flex', gap: '8px' }}>
            <button
              class="safai-btn safai-btn--ghost"
              disabled={!canGoBack()}
              onClick={() => void popTo(stack().length - 2)}
              aria-label="Back"
            >
              <Icon name="arrowLeft" size={12} /> Back
            </button>
            <button
              class="safai-btn safai-btn--ghost"
              onClick={() => void rescan()}
              aria-label="Rescan"
            >
              <Icon name="refresh" size={12} /> Rescan
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
        <HeaderStrip
          response={resp()}
          primary={primary()}
          breadcrumb={stack()}
          scanning={scanning()}
          permStatus={permStatus() ?? []}
          onHome={() => void goHome()}
          onPop={(idx) => void popTo(idx)}
        />

        <Show when={error()}>
          <ErrorCard message={error()!} onRetry={() => void navigateTo(currentRoot())} />
        </Show>

        <div
          style={{
            display: 'grid',
            'grid-template-columns': 'minmax(0, 1fr) 320px',
            gap: '18px',
            'align-items': 'stretch',
            flex: 1,
            'min-height': '520px',
          }}
        >
          <TreemapCanvas
            response={resp()}
            scanning={scanning()}
            onTileClick={(t) => void drillInto(t)}
          />
          <BiggestFoldersPanel
            folders={resp()?.biggest ?? []}
            total={resp()?.totalBytes ?? 0}
            scanning={scanning() && !resp()}
            disabled={scanning()}
            onFolderClick={(f) => {
              if (scanning()) return;
              setStack((s) => [...s, f.path]);
              setResponse(null);
              void navigateTo(f.path);
            }}
          />
        </div>
      </div>
    </div>
  );
}

// header

function HeaderStrip(props: {
  response: TreemapResponse | null;
  primary: Volume | null;
  breadcrumb: string[];
  scanning: boolean;
  permStatus: PermissionStatusEntry[];
  onHome: () => void;
  onPop: (idx: number) => void;
}) {
  const split = () => splitBytes(props.response?.totalBytes ?? 0);
  const atHome = () => props.breadcrumb.length === 0;
  const scopeLabel = () => (atHome() ? 'Home folder' : 'This folder');

  // reconciliation math. accounted = current subtree bytes (or home on
  // top-level), unaccounted = volume.used - accounted, floored at 0 so
  // drilling into a subfolder doesn't show a negative band
  const accounted = () => props.response?.totalBytes ?? 0;
  const totalBytes = () => props.primary?.totalBytes ?? 0;
  const usedBytes = () => props.primary?.usedBytes ?? 0;
  const freeBytes = () => props.primary?.freeBytes ?? 0;
  const unaccounted = () =>
    atHome() ? Math.max(0, usedBytes() - accounted()) : Math.max(0, usedBytes() - accounted());

  // mac FDA: show CTA when permission is explicitly denied AND
  // unaccounted is meaningful (>5% of volume). non-mac permStatus has
  // no entry for this kind so the find() returns undefined.
  const fdaDenied = () =>
    props.permStatus.find((p) => p.kind === 'mac-full-disk-access')?.status === 'denied';
  const showFdaCta = () =>
    atHome() &&
    fdaDenied() &&
    totalBytes() > 0 &&
    unaccounted() / totalBytes() > 0.05;

  return (
    <div
      class="safai-card safai-sheen"
      style={{
        padding: '20px 24px',
        display: 'flex',
        'flex-direction': 'column',
        gap: '14px',
        background: 'linear-gradient(135deg, oklch(0.22 0.02 240), oklch(0.20 0.02 260))',
        border: '1px solid oklch(0.82 0.14 200 / 0.25)',
      }}
    >
      <div style={{ display: 'flex', 'align-items': 'center', gap: '20px' }}>
        <Suds size={64} mood="happy" float={props.scanning} />
        <div style={{ flex: 1, 'min-width': 0 }}>
          <Breadcrumb
            root={props.response?.root ?? null}
            stack={props.breadcrumb}
            onHome={props.onHome}
            onPop={props.onPop}
          />
          <div
            style={{
              display: 'flex',
              'align-items': 'baseline',
              gap: '10px',
              'margin-top': '6px',
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
              in {scopeLabel().toLowerCase()} · {formatCount(props.response?.totalFiles ?? 0)} files
            </div>
            <Show when={props.scanning}>
              <ScanningPill />
            </Show>
          </div>
        </div>
        <Show when={props.primary}>
          {(vol) => (
            <div style={{ 'text-align': 'right', 'min-width': '120px' }}>
              <div
                style={{
                  'font-size': '10px',
                  color: 'var(--safai-fg-3)',
                  'letter-spacing': '0.12em',
                  'text-transform': 'uppercase',
                  'margin-bottom': '4px',
                }}
              >
                {vol().name} · whole disk
              </div>
              <div class="num" style={{ 'font-size': '14px', color: 'var(--safai-fg-0)' }}>
                {formatBytes(vol().usedBytes)} used
              </div>
              <div
                class="num"
                style={{ 'font-size': '11px', color: 'var(--safai-fg-2)', 'margin-top': '2px' }}
              >
                {formatBytes(vol().freeBytes)} free of {formatBytes(vol().totalBytes)}
              </div>
            </div>
          )}
        </Show>
      </div>

      <Show when={atHome() && totalBytes() > 0}>
        <ReconciliationBar
          accounted={accounted()}
          unaccounted={unaccounted()}
          free={freeBytes()}
          total={totalBytes()}
        />
      </Show>

      <Show when={showFdaCta()}>
        <FdaCta />
      </Show>
    </div>
  );
}

// three-band stacked bar: what we scanned, what the OS has that we
// can't read (system / other users / snapshots), and free space
function ReconciliationBar(props: {
  accounted: number;
  unaccounted: number;
  free: number;
  total: number;
}) {
  const pct = (v: number) =>
    props.total > 0 ? `${Math.max(0, Math.min(100, (v / props.total) * 100)).toFixed(2)}%` : '0%';
  const systemTip = protectedTooltip();
  return (
    <div style={{ display: 'flex', 'flex-direction': 'column', gap: '6px' }}>
      <div
        style={{
          height: '8px',
          display: 'flex',
          'border-radius': '999px',
          overflow: 'hidden',
          background: 'var(--safai-bg-2)',
          border: '1px solid var(--safai-line)',
        }}
      >
        <div
          style={{ width: pct(props.accounted), background: 'var(--safai-cyan)' }}
          title={`Scanned by safai: ${formatBytes(props.accounted)}`}
        />
        <div
          style={{
            width: pct(props.unaccounted),
            background: 'oklch(0.58 0.10 60)',
          }}
          title={`System & protected: ${formatBytes(props.unaccounted)} - ${systemTip}`}
        />
        <div
          style={{
            width: pct(props.free),
            background: 'oklch(0.35 0.02 240)',
          }}
          title={`Free: ${formatBytes(props.free)}`}
        />
      </div>
      <div
        style={{
          display: 'flex',
          gap: '16px',
          'font-size': '10px',
          color: 'var(--safai-fg-3)',
          'letter-spacing': '0.02em',
          'flex-wrap': 'wrap',
        }}
      >
        <LegendDot color="var(--safai-cyan)" label={`Scanned ${formatBytes(props.accounted)}`} />
        <LegendDot
          color="oklch(0.58 0.10 60)"
          label={`System & protected ${formatBytes(props.unaccounted)}`}
          title={systemTip}
        />
        <LegendDot color="oklch(0.35 0.02 240)" label={`Free ${formatBytes(props.free)}`} />
      </div>
    </div>
  );
}

function LegendDot(props: { color: string; label: string; title?: string }) {
  return (
    <span
      title={props.title}
      style={{ display: 'inline-flex', 'align-items': 'center', gap: '6px' }}
    >
      <span
        style={{
          width: '8px',
          height: '8px',
          'border-radius': '2px',
          background: props.color,
        }}
      />
      <span>{props.label}</span>
    </span>
  );
}

// best-effort per-OS copy. detection via userAgent since we don't want
// to pay for a tauri round-trip here - it's purely informational
function protectedTooltip(): string {
  const ua = typeof navigator !== 'undefined' ? navigator.userAgent : '';
  if (/Mac|Darwin/i.test(ua)) {
    return 'macOS system files, other users, APFS snapshots';
  }
  if (/Win/i.test(ua)) {
    return 'Windows system files, other users, System Volume Information';
  }
  return 'system files owned by root, other users';
}

function FdaCta() {
  const onClick = async () => {
    try {
      await openPermissionSettings('mac-full-disk-access');
    } catch {
      // user can still navigate System Settings manually
    }
  };
  return (
    <div
      style={{
        display: 'flex',
        'align-items': 'center',
        gap: '10px',
        padding: '8px 12px',
        'border-radius': '6px',
        background: 'oklch(0.82 0.14 60 / 0.12)',
        border: '1px solid oklch(0.82 0.14 60 / 0.35)',
        'font-size': '11px',
        color: 'var(--safai-fg-1)',
      }}
    >
      <Icon name="refresh" size={12} />
      <span style={{ flex: 1 }}>
        Grant Full Disk Access to shrink the "System & protected" band.
      </span>
      <button
        class="safai-btn safai-btn--ghost"
        style={{ padding: '2px 10px', 'font-size': '11px' }}
        onClick={() => void onClick()}
      >
        Open Settings
      </button>
    </div>
  );
}

function ScanningPill() {
  return (
    <div
      style={{
        'margin-left': '12px',
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
      Scanning
    </div>
  );
}

function Breadcrumb(props: {
  root: string | null;
  stack: string[];
  onHome: () => void;
  onPop: (idx: number) => void;
}) {
  const chain = createMemo(() => {
    const full = props.stack;
    if (full.length === 0) {
      return [{ label: labelOf(props.root ?? '/'), idx: -1 }];
    }
    const tail = full.slice(-3);
    return tail.map((p, i) => ({
      label: labelOf(p),
      idx: full.length - tail.length + i,
    }));
  });
  return (
    <div style={{ display: 'flex', 'align-items': 'center', gap: '6px', 'flex-wrap': 'wrap' }}>
      <button
        class="safai-btn safai-btn--ghost"
        style={{ padding: '2px 8px', 'font-size': '11px' }}
        onClick={props.onHome}
        aria-label="Home"
      >
        <Icon name="archive" size={10} /> Home
      </button>
      <For each={chain()}>
        {(c) => (
          <>
            <span style={{ color: 'var(--safai-fg-3)', 'font-size': '11px' }}>/</span>
            <button
              class="safai-btn safai-btn--ghost"
              style={{ padding: '2px 8px', 'font-size': '11px' }}
              onClick={() => (c.idx < 0 ? props.onHome() : props.onPop(c.idx))}
              title={c.label}
            >
              {c.label}
            </button>
          </>
        )}
      </For>
    </div>
  );
}

function labelOf(path: string): string {
  if (!path) return '/';
  const norm = path.replace(/[\\/]+$/, '');
  const last = norm.split(/[\\/]/).pop();
  return last && last.length > 0 ? last : path;
}

// treemap canvas

// html treemap, each tile a % sized absolute div. no relayout on resize
// needed, browser does the pixel math from %.
function TreemapCanvas(props: {
  response: TreemapResponse | null;
  scanning: boolean;
  onTileClick: (tile: TreemapTile) => void;
}) {
  const tiles = () => props.response?.tiles ?? [];
  const [hover, setHover] = createSignal<TreemapTile | null>(null);
  const showEmpty = () =>
    !props.scanning && props.response != null && tiles().length === 0;

  return (
    <div
      class="safai-card"
      style={{
        padding: '18px',
        display: 'flex',
        'flex-direction': 'column',
        gap: '10px',
        'min-height': '520px',
      }}
    >
      <div style={{ display: 'flex', 'align-items': 'center', gap: '12px' }}>
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-2)',
            'letter-spacing': '0.1em',
            'text-transform': 'uppercase',
          }}
        >
          Treemap
        </div>
        <Show when={hover()}>
          {(t) => (
            <div
              class="mono"
              style={{
                'font-size': '11px',
                color: 'var(--safai-fg-1)',
                'overflow': 'hidden',
                'text-overflow': 'ellipsis',
                'white-space': 'nowrap',
                flex: 1,
              }}
            >
              {t().name} · {formatBytes(t().bytes)} · {formatCount(t().fileCount)} files
            </div>
          )}
        </Show>
        <Show when={props.response}>
          {(r) => (
            <div style={{ 'font-size': '10px', color: 'var(--safai-fg-3)' }}>
              {props.scanning
                ? 'scanning…'
                : `scanned ${formatRelativeTime(r().scannedAt, Date.now())} in ${r().durationMs}ms`}
            </div>
          )}
        </Show>
      </div>

      <Show when={showEmpty()}>
        <EmptyCanvas />
      </Show>

      <Show when={!showEmpty()}>
        <div
          style={{
            flex: 1,
            position: 'relative',
            'min-height': '480px',
            background: 'var(--safai-bg-2)',
            'border-radius': '8px',
            overflow: 'hidden',
          }}
        >
          <For each={tiles()}>
            {(tile) => (
              <TreemapRect
                tile={tile}
                disabled={props.scanning}
                onClick={() => props.onTileClick(tile)}
                onHover={(t) => setHover(t)}
              />
            )}
          </For>
          <Show when={props.scanning && tiles().length === 0}>
            <LoadingOverlay />
          </Show>
        </div>
      </Show>
    </div>
  );
}

function TreemapRect(props: {
  tile: TreemapTile;
  disabled?: boolean;
  onClick: () => void;
  onHover: (t: TreemapTile | null) => void;
}) {
  const { tile } = props;
  const color = () => (tile.isOther ? 'oklch(0.35 0.02 240)' : tileColor(tile.name));
  const pct = (v: number) => `${(v * 100).toFixed(3)}%`;
  const clickable = () => tile.isDir && !tile.isOther && !props.disabled;

  return (
    <div
      role={clickable() ? 'button' : 'img'}
      tabindex={clickable() ? 0 : -1}
      aria-disabled={props.disabled ? 'true' : undefined}
      onClick={() => clickable() && props.onClick()}
      onMouseEnter={() => props.onHover(tile)}
      onMouseLeave={() => props.onHover(null)}
      onKeyDown={(e) => {
        if (!clickable()) return;
        if (e.key === 'Enter' || e.key === ' ') props.onClick();
      }}
      title={
        props.disabled
          ? 'Wait for the scan to finish before drilling in'
          : `${tile.name} - ${formatBytes(tile.bytes)} (${formatCount(tile.fileCount)} files)`
      }
      style={{
        position: 'absolute',
        left: pct(tile.rect.x),
        top: pct(tile.rect.y),
        width: pct(tile.rect.w),
        height: pct(tile.rect.h),
        background: color(),
        opacity: tile.isOther ? 0.55 : 0.92,
        border: '1px solid oklch(0.14 0.02 240 / 0.6)',
        cursor: clickable() ? 'pointer' : 'default',
        overflow: 'hidden',
        display: 'flex',
        'flex-direction': 'column',
        'align-items': 'flex-start',
        'justify-content': 'flex-start',
        padding: '6px 8px',
        'box-sizing': 'border-box',
        color: 'oklch(0.99 0 0)',
        'text-shadow': '0 1px 2px oklch(0.14 0.02 240 / 0.6)',
        'font-size': '11px',
        'line-height': 1.15,
        transition: 'opacity 120ms ease, transform 120ms ease',
      }}
    >
      <div
        style={{
          'font-weight': 500,
          'white-space': 'nowrap',
          'overflow': 'hidden',
          'text-overflow': 'ellipsis',
          'max-width': '100%',
          'font-size': '11px',
          'letter-spacing': '-0.01em',
        }}
      >
        {tile.name}
      </div>
      <div
        class="num"
        style={{
          'font-size': '10px',
          opacity: 0.85,
          'font-variant-numeric': 'tabular-nums',
          'white-space': 'nowrap',
          'overflow': 'hidden',
          'text-overflow': 'ellipsis',
          'max-width': '100%',
        }}
      >
        {formatBytes(tile.bytes)}
      </div>
    </div>
  );
}

function EmptyCanvas() {
  return (
    <div
      style={{
        flex: 1,
        display: 'flex',
        'flex-direction': 'column',
        'align-items': 'center',
        'justify-content': 'center',
        gap: '12px',
        color: 'var(--safai-fg-3)',
        'font-size': '12px',
        'min-height': '360px',
      }}
    >
      <Suds size={80} mood="sleepy" />
      <div>Nothing to render - this folder is empty.</div>
    </div>
  );
}

function LoadingOverlay() {
  return (
    <div
      style={{
        position: 'absolute',
        inset: 0,
        display: 'flex',
        'align-items': 'center',
        'justify-content': 'center',
        background: 'oklch(0.14 0.02 240 / 0.3)',
      }}
    >
      <div
        style={{
          'font-size': '12px',
          color: 'var(--safai-fg-1)',
          background: 'var(--safai-bg-1)',
          padding: '10px 16px',
          'border-radius': '999px',
          border: '1px solid var(--safai-line)',
        }}
      >
        <Icon name="refresh" size={11} /> Walking the tree…
      </div>
    </div>
  );
}

// biggest folders sidebar

function BiggestFoldersPanel(props: {
  folders: BiggestFolder[];
  total: number;
  scanning: boolean;
  disabled?: boolean;
  onFolderClick: (f: BiggestFolder) => void;
}) {
  return (
    <div
      class="safai-card"
      style={{
        padding: '18px',
        display: 'flex',
        'flex-direction': 'column',
        gap: '10px',
        'min-height': '520px',
        'overflow-y': 'auto',
      }}
    >
      <div
        style={{
          'font-size': '11px',
          color: 'var(--safai-fg-2)',
          'letter-spacing': '0.1em',
          'text-transform': 'uppercase',
        }}
      >
        Biggest folders
      </div>
      <Show
        when={props.folders.length > 0}
        fallback={
          <Show when={!props.scanning} fallback={<SkeletonList />}>
            <div style={{ 'font-size': '12px', color: 'var(--safai-fg-3)' }}>
              No subfolders to list.
            </div>
          </Show>
        }
      >
        <For each={props.folders}>
          {(f) => (
            <BiggestFolderRow
              f={f}
              total={props.total}
              disabled={props.disabled ?? false}
              onClick={() => props.onFolderClick(f)}
            />
          )}
        </For>
      </Show>
    </div>
  );
}

function BiggestFolderRow(props: {
  f: BiggestFolder;
  total: number;
  disabled: boolean;
  onClick: () => void;
}) {
  const pct = () => (props.total > 0 ? (props.f.bytes / props.total) * 100 : 0);
  return (
    <button
      class="safai-card safai-card--hover"
      disabled={props.disabled}
      title={props.disabled ? 'Wait for the scan to finish' : props.f.path}
      style={{
        padding: '10px 12px',
        cursor: props.disabled ? 'default' : 'pointer',
        background: 'var(--safai-bg-2)',
        border: '1px solid var(--safai-line)',
        'text-align': 'left',
        opacity: props.disabled ? 0.6 : 1,
      }}
      onClick={props.onClick}
    >
      <div
        style={{
          display: 'flex',
          'align-items': 'baseline',
          'justify-content': 'space-between',
          gap: '8px',
        }}
      >
        <div
          style={{
            'font-size': '12px',
            color: 'var(--safai-fg-0)',
            'overflow': 'hidden',
            'text-overflow': 'ellipsis',
            'white-space': 'nowrap',
            flex: 1,
          }}
          title={props.f.path}
        >
          {props.f.name}
        </div>
        <div
          class="num"
          style={{
            'font-size': '12px',
            color: 'var(--safai-fg-1)',
            'font-variant-numeric': 'tabular-nums',
          }}
        >
          {formatBytes(props.f.bytes)}
        </div>
      </div>
      <div
        style={{
          height: '3px',
          background: 'var(--safai-bg-3)',
          'border-radius': '2px',
          overflow: 'hidden',
          'margin-top': '6px',
        }}
      >
        <div
          style={{
            height: '100%',
            width: `${Math.min(100, pct()).toFixed(1)}%`,
            background: tileColor(props.f.name),
          }}
        />
      </div>
      <div
        style={{
          'font-size': '10px',
          color: 'var(--safai-fg-3)',
          'margin-top': '4px',
          display: 'flex',
          gap: '8px',
        }}
      >
        <span>{pct().toFixed(1)}%</span>
        <span>·</span>
        <span>{formatCount(props.f.fileCount)} files</span>
        <span>·</span>
        <span>depth {props.f.depth}</span>
      </div>
    </button>
  );
}

function SkeletonList() {
  return (
    <For each={Array.from({ length: 8 })}>
      {() => (
        <div
          style={{
            height: '52px',
            'border-radius': '6px',
            background:
              'linear-gradient(90deg, var(--safai-bg-2) 0%, var(--safai-bg-3) 50%, var(--safai-bg-2) 100%)',
            'background-size': '200% 100%',
            animation: 'safai-shimmer 1.4s ease-in-out infinite',
          }}
        />
      )}
    </For>
  );
}

// error

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
          Couldn't build the treemap
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
