import { createMemo, createSignal, onCleanup, onMount, Show, type JSX } from 'solid-js';
import { Icon } from './Icon';
import { cacheFreshness, type ScanCacheKey } from '../lib/scanCache';
import { formatRelativeTime, truncateMiddle } from '../lib/format';

interface CacheFreshnessProps {
  cacheKey: ScanCacheKey;
  version?: unknown;
  disabled?: boolean;
  onRescan?: () => void;
}

export function CacheFreshness(props: CacheFreshnessProps): JSX.Element {
  const [now, setNow] = createSignal(Date.now());

  onMount(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    onCleanup(() => window.clearInterval(timer));
  });

  const freshness = createMemo(() => {
    void props.version;
    return cacheFreshness(props.cacheKey, now());
  });
  const meta = () => freshness().metadata;
  const stale = () => freshness().stale;
  const relative = () => {
    const m = meta();
    return m ? formatRelativeTime(Math.floor(m.cachedAtMs / 1000), now()) : '';
  };
  const title = () => {
    const m = meta();
    if (!m) return undefined;
    const options = Object.entries(m.options)
      .map(([key, value]) => `${key}=${String(value)}`)
      .join(', ');
    return [
      `Cached: ${new Date(m.cachedAtMs).toLocaleString()}`,
      `Scanned: ${new Date(m.scannedAtMs).toLocaleString()}`,
      m.rootPath ? `Scope: ${m.rootPath}` : null,
      options ? `Options: ${options}` : null,
    ]
      .filter(Boolean)
      .join('\n');
  };

  return (
    <Show when={meta()}>
      {(m) => (
        <span
          title={title()}
          style={{
            display: 'inline-flex',
            'align-items': 'center',
            gap: '8px',
            'flex-wrap': 'wrap',
          }}
        >
          <span
            style={{
              display: 'inline-flex',
              'align-items': 'center',
              gap: '5px',
              color: stale() ? 'oklch(0.76 0.13 75)' : 'var(--safai-fg-3)',
            }}
          >
            <Icon name={stale() ? 'warning' : 'clock'} size={11} />
            {stale() ? `Cached ${relative()} - refresh suggested` : `Cached ${relative()}`}
          </span>
          <Show when={m().rootPath}>
            {(root) => (
              <span style={{ color: 'var(--safai-fg-3)' }}>
                Scope: {truncateMiddle(root(), 34)}
              </span>
            )}
          </Show>
          <Show when={stale() && props.onRescan}>
            <button
              class="safai-btn safai-btn--ghost"
              style={{ height: '22px', 'font-size': '11px', padding: '0 8px' }}
              disabled={props.disabled}
              onClick={() => props.onRescan?.()}
            >
              <Icon name="refresh" size={10} /> Refresh
            </button>
          </Show>
        </span>
      )}
    </Show>
  );
}
