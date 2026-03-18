import { createMemo, createResource, createSignal, For, onCleanup, Show } from 'solid-js';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon } from '../components/Icon';
import {
  cancelTreemap,
  forgetTreemap,
  invalidateTreemapCache,
  serveTreemapSubtree,
  startTreemap,
  subscribeTreemap,
  tileColor,
  type BiggestFolder,
  type TreemapResponse,
  type TreemapTile,
} from '../lib/treemap';
import { listVolumes, pickPrimary, type Volume } from '../lib/volumes';
import { formatBytes, formatCount, splitBytes } from '../lib/format';

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

  let currentHandle: string | null = null;
  let unsubscribe: (() => void) | null = null;

  const startWalk = async (root: string | undefined) => {
    await stopWalk();
    setError(null);
    setScanning(true);

    try {
      unsubscribe = await subscribeTreemap({
        onProgress: (r) => setResponse(r),
        onDone: (r) => {
          setResponse(r);
          setScanning(false);
        },
      });
      const handle = await startTreemap({ root, depth: 4 });
      currentHandle = handle.id;
    } catch (e) {
      setError(String(e));
      setScanning(false);
    }
  };

  const stopWalk = async () => {
    if (currentHandle) {
      try {
        await cancelTreemap(currentHandle);
        await forgetTreemap(currentHandle);
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

  // try cache first, fall back to a streaming walk. the rust side seeds
  // the cache on done so drill/back hits next time. this is what fixes
  // "it rescans when i go back to Home"
  const navigateTo = async (root: string | undefined) => {
    try {
      const cached = await serveTreemapSubtree(root);
      if (cached) {
        // kill any in-flight walk, its emit_done would clobber the cached
        // response with older/wider state
        await stopWalk();
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
  void navigateTo(undefined);

  onCleanup(() => {
    void stopWalk();
  });

  const drillInto = async (tile: TreemapTile) => {
    if (!tile.isDir || tile.isOther) return;
    setStack((s) => [...s, tile.path]);
    setResponse(null);
    await navigateTo(tile.path);
  };

  const popTo = async (idx: number) => {
    setStack((s) => s.slice(0, idx + 1));
    setResponse(null);
    await navigateTo(currentRoot());
  };

  const goHome = async () => {
    setStack([]);
    setResponse(null);
    await navigateTo(undefined);
  };

  // user-clicked rescan. nuke rust cache first so the fresh walk isn't
  // short-circuited, then restart from current root
  const rescan = async () => {
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
        subtitle="Find out what's really taking up space — every rectangle is a folder."
        right={
          <div style={{ display: 'flex', gap: '8px' }}>
            <button
              class="safai-btn safai-btn--ghost"
              disabled={!canGoBack()}
              onClick={() => void popTo(stack().length - 2)}
              aria-label="Back"
            >
              <Icon name="refresh" size={12} /> Back
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
            onFolderClick={(f) => {
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
  onHome: () => void;
  onPop: (idx: number) => void;
}) {
  const split = () => splitBytes(props.response?.totalBytes ?? 0);
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
      <Suds size={64} mood={props.scanning ? 'happy' : 'happy'} float={props.scanning} />
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
            across {formatCount(props.response?.totalFiles ?? 0)} files
          </div>
          <Show when={props.scanning}>
            <ScanningPill />
          </Show>
        </div>
      </div>
      <Show when={props.primary}>
        {(vol) => (
          <div style={{ 'text-align': 'right' }}>
            <div
              style={{
                'font-size': '10px',
                color: 'var(--safai-fg-3)',
                'letter-spacing': '0.12em',
                'text-transform': 'uppercase',
                'margin-bottom': '4px',
              }}
            >
              {vol().name}
            </div>
            <div class="num" style={{ 'font-size': '14px', color: 'var(--safai-fg-1)' }}>
              {formatBytes(vol().freeBytes)} free
            </div>
          </div>
        )}
      </Show>
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
              {props.scanning ? 'scanning…' : `scanned in ${r().durationMs}ms`}
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
  onClick: () => void;
  onHover: (t: TreemapTile | null) => void;
}) {
  const { tile } = props;
  const color = () => (tile.isOther ? 'oklch(0.35 0.02 240)' : tileColor(tile.name));
  const pct = (v: number) => `${(v * 100).toFixed(3)}%`;

  return (
    <div
      role={tile.isDir && !tile.isOther ? 'button' : 'img'}
      tabindex={tile.isDir && !tile.isOther ? 0 : -1}
      onClick={props.onClick}
      onMouseEnter={() => props.onHover(tile)}
      onMouseLeave={() => props.onHover(null)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') props.onClick();
      }}
      title={`${tile.name} — ${formatBytes(tile.bytes)} (${formatCount(tile.fileCount)} files)`}
      style={{
        position: 'absolute',
        left: pct(tile.rect.x),
        top: pct(tile.rect.y),
        width: pct(tile.rect.w),
        height: pct(tile.rect.h),
        background: color(),
        opacity: tile.isOther ? 0.55 : 0.92,
        border: '1px solid oklch(0.14 0.02 240 / 0.6)',
        cursor: tile.isDir && !tile.isOther ? 'pointer' : 'default',
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
      <div>Nothing to render — this folder is empty.</div>
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
            <BiggestFolderRow f={f} total={props.total} onClick={() => props.onFolderClick(f)} />
          )}
        </For>
      </Show>
    </div>
  );
}

function BiggestFolderRow(props: {
  f: BiggestFolder;
  total: number;
  onClick: () => void;
}) {
  const pct = () => (props.total > 0 ? (props.f.bytes / props.total) * 100 : 0);
  return (
    <button
      class="safai-card safai-card--hover"
      style={{
        padding: '10px 12px',
        cursor: 'pointer',
        background: 'var(--safai-bg-2)',
        border: '1px solid var(--safai-line)',
        'text-align': 'left',
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
