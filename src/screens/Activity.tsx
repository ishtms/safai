import {
  createMemo,
  createSignal,
  For,
  onCleanup,
  onMount,
  Show,
  type JSX,
} from 'solid-js';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon } from '../components/Icon';
import { formatBytes, formatCount } from '../lib/format';
import {
  filterProcesses,
  killProcess,
  sortProcesses,
  type ProcessRow,
  type SortDirection,
  type SortKey,
} from '../lib/activity';
import {
  acquireActivityStream,
  latestSnapshot,
  releaseActivityStream,
} from '../lib/activityStream';

// activity monitor. sortable table off the activity://snapshot stream.
// filter is client-side on the latest snapshot. kill is SIGTERM, alt-click = SIGKILL.

export default function Activity(): JSX.Element {
  // shared stream across Memory + Activity + remounts. no handle juggling
  // here, the singleton owns it
  const snap = latestSnapshot();
  const [error] = createSignal<string | null>(null);
  const [query, setQuery] = createSignal('');
  const [sortKey, setSortKey] = createSignal<SortKey>('cpu');
  const [sortDir, setSortDir] = createSignal<SortDirection>('desc');
  const [selected, setSelected] = createSignal<number | null>(null);
  const [pendingKill, setPendingKill] = createSignal<Set<number>>(new Set());
  const [status, setStatus] = createSignal<
    { tone: 'ok' | 'error'; text: string } | null
  >(null);

  onMount(() => {
    acquireActivityStream();
  });

  onCleanup(() => {
    releaseActivityStream();
  });

  const sortedRows = createMemo<ProcessRow[]>(() => {
    const s = snap();
    if (!s) return [];
    const filtered = filterProcesses(s.processes, query());
    return sortProcesses(filtered, sortKey(), sortDir());
  });

  // cap rendered rows, busy servers hit 10k+ and that stalls the main thread.
  // 100 is enough to spot the runaway, toolbar still shows full count
  const VISIBLE_CAP = 100;
  const visibleRows = createMemo<ProcessRow[]>(() =>
    sortedRows().slice(0, VISIBLE_CAP),
  );
  const hiddenCount = createMemo(() =>
    Math.max(0, sortedRows().length - VISIBLE_CAP),
  );

  const selectedRow = createMemo<ProcessRow | null>(() => {
    const pid = selected();
    if (pid == null) return null;
    return snap()?.processes.find((p) => p.pid === pid) ?? null;
  });

  function setSort(k: SortKey) {
    if (sortKey() === k) {
      setSortDir((d) => (d === 'desc' ? 'asc' : 'desc'));
    } else {
      setSortKey(k);
      setSortDir('desc');
    }
  }

  async function onKill(row: ProcessRow, force: boolean) {
    if (pendingKill().has(row.pid)) return;
    setPendingKill((prev) => {
      const next = new Set(prev);
      next.add(row.pid);
      return next;
    });
    try {
      await killProcess(row.pid, force);
      setStatus({
        tone: 'ok',
        text: `Sent ${force ? 'SIGKILL' : 'SIGTERM'} to ${row.name} (pid ${row.pid}).`,
      });
      if (selected() === row.pid) setSelected(null);
    } catch (e) {
      setStatus({ tone: 'error', text: `Couldn't kill ${row.name}: ${String(e)}` });
    } finally {
      setPendingKill((prev) => {
        const next = new Set(prev);
        next.delete(row.pid);
        return next;
      });
    }
  }

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Maintenance"
        title="Activity"
        subtitle="Live CPU + memory usage per process. Kill runaway apps."
        right={
          <div style={{ display: 'flex', gap: '10px', 'align-items': 'center' }}>
            <Show when={snap()}>
              {(s) => (
                <span style={{ 'font-size': '12px', color: 'var(--safai-fg-3)' }}>
                  <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
                    {formatCount(s().processCount)}
                  </span>{' '}
                  processes ·{' '}
                  <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
                    {s().cpu.averagePercent.toFixed(0)}%
                  </span>{' '}
                  CPU
                </span>
              )}
            </Show>
            <div
              style={{
                display: 'flex',
                'align-items': 'center',
                gap: '6px',
                padding: '4px 10px',
                'border-radius': 'var(--safai-r-md)',
                background: 'var(--safai-bg-2)',
              }}
            >
              <Icon name="search" size={11} color="var(--safai-fg-3)" />
              <input
                value={query()}
                onInput={(e) => setQuery(e.currentTarget.value)}
                placeholder="Filter by name, pid, or command"
                style={{
                  border: 'none',
                  background: 'transparent',
                  color: 'var(--safai-fg-0)',
                  'font-size': '12px',
                  width: '240px',
                  outline: 'none',
                }}
              />
            </div>
          </div>
        }
      />

      <div
        style={{
          flex: 1,
          overflow: 'hidden',
          padding: '20px 24px',
          display: 'grid',
          'grid-template-columns': 'minmax(0, 1fr) 280px',
          gap: '16px',
        }}
      >
        <div
          class="safai-card"
          style={{ overflow: 'hidden', display: 'flex', 'flex-direction': 'column' }}
        >
          <div
            style={{
              display: 'grid',
              'grid-template-columns': '1fr 80px 110px 110px 110px',
              padding: '10px 16px',
              'border-bottom': '1px solid var(--safai-line)',
              'font-size': '10px',
              color: 'var(--safai-fg-3)',
              'letter-spacing': '0.12em',
              'text-transform': 'uppercase',
              gap: '12px',
            }}
          >
            <HeaderCell
              label="Process"
              active={sortKey() === 'name'}
              dir={sortDir()}
              onClick={() => setSort('name')}
              align="left"
            />
            <HeaderCell
              label="PID"
              active={sortKey() === 'pid'}
              dir={sortDir()}
              onClick={() => setSort('pid')}
              align="right"
            />
            <HeaderCell
              label="CPU"
              active={sortKey() === 'cpu'}
              dir={sortDir()}
              onClick={() => setSort('cpu')}
              align="right"
            />
            <HeaderCell
              label="Memory"
              active={sortKey() === 'memory'}
              dir={sortDir()}
              onClick={() => setSort('memory')}
              align="right"
            />
            <span style={{ 'text-align': 'right' }}>Action</span>
          </div>
          <div style={{ 'overflow-y': 'auto', flex: 1 }}>
            <Show
              when={visibleRows().length > 0}
              fallback={
                <div style={{ padding: '40px 16px', color: 'var(--safai-fg-3)', 'font-size': '12px' }}>
                  No processes match that filter.
                </div>
              }
            >
              <For each={visibleRows()}>
                {(row) => (
                  <ProcessRowView
                    row={row}
                    selected={selected() === row.pid}
                    pending={pendingKill().has(row.pid)}
                    onSelect={() => setSelected(row.pid)}
                    onKill={(force) => onKill(row, force)}
                  />
                )}
              </For>
              <Show when={hiddenCount() > 0}>
                <div
                  style={{
                    padding: '10px 16px',
                    'font-size': '11px',
                    color: 'var(--safai-fg-3)',
                    'text-align': 'center',
                  }}
                >
                  Showing top {VISIBLE_CAP} of {formatCount(sortedRows().length)} ·
                  narrow the filter or change the sort to surface more.
                </div>
              </Show>
            </Show>
          </div>
        </div>

        <DetailPane row={selectedRow()} />
      </div>

      <Show when={error()}>
        {(e) => (
          <div
            style={{
              padding: '8px 24px',
              'font-size': '11px',
              color: 'var(--safai-coral)',
              'border-top': '1px solid var(--safai-line)',
            }}
          >
            {e()}
          </div>
        )}
      </Show>

      <Show when={status()}>
        {(s) => (
          <StatusBanner
            tone={s().tone}
            text={s().text}
            onDismiss={() => setStatus(null)}
          />
        )}
      </Show>
    </div>
  );
}

// sub-components

function HeaderCell(props: {
  label: string;
  active: boolean;
  dir: SortDirection;
  onClick: () => void;
  align: 'left' | 'right';
}) {
  return (
    <button
      onClick={props.onClick}
      class="safai-btn safai-btn--ghost"
      style={{
        background: 'transparent',
        'border-color': 'transparent',
        padding: '0',
        'font-size': '10px',
        'letter-spacing': '0.12em',
        'text-transform': 'uppercase',
        color: props.active ? 'var(--safai-fg-1)' : 'var(--safai-fg-3)',
        'justify-content': props.align === 'right' ? 'flex-end' : 'flex-start',
        cursor: 'pointer',
        height: '16px',
      }}
    >
      {props.label}
      <Show when={props.active}>
        <span style={{ 'margin-left': '4px', 'font-size': '9px' }}>
          {props.dir === 'desc' ? '▼' : '▲'}
        </span>
      </Show>
    </button>
  );
}

function ProcessRowView(props: {
  row: ProcessRow;
  selected: boolean;
  pending: boolean;
  onSelect: () => void;
  onKill: (force: boolean) => void;
}) {
  return (
    <div
      role="button"
      onClick={props.onSelect}
      style={{
        display: 'grid',
        'grid-template-columns': '1fr 80px 110px 110px 110px',
        padding: '8px 16px',
        'border-bottom': '1px solid var(--safai-line)',
        'font-size': '12px',
        color: 'var(--safai-fg-1)',
        gap: '12px',
        'align-items': 'center',
        background: props.selected ? 'var(--safai-bg-2)' : 'transparent',
        cursor: 'pointer',
      }}
    >
      <span
        style={{
          'white-space': 'nowrap',
          overflow: 'hidden',
          'text-overflow': 'ellipsis',
        }}
        title={props.row.command || props.row.name}
      >
        {props.row.name}
      </span>
      <span class="num" style={{ 'text-align': 'right', color: 'var(--safai-fg-2)' }}>
        {props.row.pid}
      </span>
      <span class="num" style={{ 'text-align': 'right' }}>
        {Number.isNaN(props.row.cpuPercent) ? '-' : `${props.row.cpuPercent.toFixed(1)}%`}
      </span>
      <span class="num" style={{ 'text-align': 'right' }}>
        {formatBytes(props.row.memoryBytes)}
      </span>
      <div style={{ display: 'flex', 'justify-content': 'flex-end', gap: '6px' }}>
        <button
          class="safai-btn safai-btn--ghost"
          disabled={props.pending}
          aria-busy={props.pending}
          onClick={(e) => {
            e.stopPropagation();
            props.onKill(e.altKey);
          }}
          title="Click to SIGTERM · Option/Alt-click to SIGKILL"
          style={{
            height: '22px',
            'font-size': '11px',
            padding: '0 8px',
            color: props.pending ? 'var(--safai-fg-3)' : 'var(--safai-coral)',
          }}
        >
          {props.pending ? 'Killing…' : 'Kill'}
        </button>
      </div>
    </div>
  );
}

function DetailPane(props: { row: ProcessRow | null }) {
  return (
    <div class="safai-card" style={{ padding: '18px', overflow: 'auto' }}>
      <Show
        when={props.row}
        fallback={
          <div>
            <div
              style={{
                display: 'flex',
                'align-items': 'center',
                gap: '10px',
                'margin-bottom': '10px',
              }}
            >
              <Suds size={36} mood="happy" />
              <div style={{ 'font-size': '12px', color: 'var(--safai-fg-1)', 'font-weight': 500 }}>
                Suds says
              </div>
            </div>
            <div style={{ 'font-size': '13px', color: 'var(--safai-fg-1)', 'line-height': 1.5 }}>
              Click a row to see its details. Click Kill to SIGTERM a process;
              Option-click to force SIGKILL (the OS can't refuse). Safai never
              kills pid 0 (kernel idle), pid 1 (init/launchd), or itself.
            </div>
          </div>
        }
      >
        {(row) => (
          <div>
            <div
              style={{
                'font-size': '14px',
                color: 'var(--safai-fg-0)',
                'font-weight': 500,
                'margin-bottom': '6px',
              }}
            >
              {row().name}
            </div>
            <div
              class="mono"
              style={{ 'font-size': '11px', color: 'var(--safai-fg-3)', 'margin-bottom': '12px' }}
            >
              pid {row().pid}
              <Show when={row().parentPid != null}>
                {' · parent '}{row().parentPid}
              </Show>
              <Show when={row().user}>
                {' · '}{row().user}
              </Show>
            </div>
            <KV label="CPU" value={`${row().cpuPercent.toFixed(1)}%`} />
            <KV label="Memory" value={formatBytes(row().memoryBytes)} />
            <Show when={row().threads != null}>
              <KV label="Threads" value={String(row().threads)} />
            </Show>
            <Show when={row().startTime}>
              <KV
                label="Started"
                value={new Date(row().startTime * 1000).toLocaleString()}
              />
            </Show>
            <Show when={row().command}>
              <div style={{ 'margin-top': '14px' }}>
                <div
                  style={{
                    'font-size': '10px',
                    color: 'var(--safai-fg-3)',
                    'letter-spacing': '0.12em',
                    'text-transform': 'uppercase',
                    'margin-bottom': '6px',
                  }}
                >
                  Command
                </div>
                <div
                  class="mono"
                  style={{
                    'font-size': '11px',
                    color: 'var(--safai-fg-1)',
                    'word-break': 'break-all',
                    'line-height': 1.4,
                    background: 'var(--safai-bg-2)',
                    padding: '8px 10px',
                    'border-radius': 'var(--safai-r-sm)',
                  }}
                >
                  {row().command}
                </div>
              </div>
            </Show>
          </div>
        )}
      </Show>
    </div>
  );
}

function KV(props: { label: string; value: string }) {
  return (
    <div
      style={{
        display: 'flex',
        'justify-content': 'space-between',
        'align-items': 'baseline',
        padding: '6px 0',
        'border-bottom': '1px solid var(--safai-line)',
        'font-size': '12px',
      }}
    >
      <span style={{ color: 'var(--safai-fg-3)' }}>{props.label}</span>
      <span class="num" style={{ color: 'var(--safai-fg-0)' }}>{props.value}</span>
    </div>
  );
}

function StatusBanner(props: {
  tone: 'ok' | 'error';
  text: string;
  onDismiss: () => void;
}) {
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
        background: props.tone === 'error' ? 'var(--safai-coral-dim)' : 'var(--safai-cyan-dim)',
        color: props.tone === 'error' ? 'var(--safai-coral)' : 'var(--safai-cyan)',
        'font-size': '12px',
        display: 'flex',
        'align-items': 'center',
        gap: '10px',
        'z-index': 500,
        'box-shadow': '0 12px 32px oklch(0 0 0 / 0.4)',
      }}
    >
      <Icon
        name={props.tone === 'error' ? 'warning' : 'check'}
        size={12}
        color="currentColor"
      />
      <span>{props.text}</span>
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
