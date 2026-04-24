import { For, Show, createResource } from 'solid-js';
import { A, useLocation } from '@solidjs/router';
import { NAV, type NavItem } from '../lib/nav';
import { Suds } from './Suds';
import { Icon } from './Icon';
import { listVolumes, pickPrimary, type Volume } from '../lib/volumes';
import { formatBytes } from '../lib/format';

export function SafaiSidebar() {
  const loc = useLocation();
  const [volumes] = createResource(listVolumes);
  const primary = () => {
    const list = volumes();
    return list ? pickPrimary(list) : null;
  };
  return (
    <div
      style={{
        width: '232px',
        'flex-shrink': 0,
        background: 'var(--safai-bg-1)',
        'border-right': '1px solid var(--safai-line)',
        display: 'flex',
        'flex-direction': 'column',
        padding: '16px 0 12px',
      }}
    >
      {/* brand */}
      <div style={{ padding: '0 16px 18px', display: 'flex', 'align-items': 'center', gap: '10px' }}>
        <Suds size={28} />
        <div
          style={{
            'font-family': 'var(--safai-font-display)',
            'font-size': '17px',
            'font-weight': 600,
            'letter-spacing': '-0.02em',
          }}
        >
          Safai
        </div>
        <div
          class="safai-pill"
          style={{
            'margin-left': 'auto',
            'font-size': '10px',
            padding: '2px 6px',
            background: 'oklch(0.82 0.14 200 / 0.12)',
            color: 'var(--safai-cyan)',
          }}
        >
          v0.2
        </div>
      </div>

      <div style={{ flex: 1, overflow: 'auto', padding: '0 8px' }}>
        <For each={NAV}>
          {(grp) => (
            <div style={{ 'margin-bottom': '14px' }}>
              <div
                style={{
                  padding: '8px 10px 6px',
                  'font-size': '10px',
                  'font-weight': 600,
                  'letter-spacing': '0.12em',
                  color: 'var(--safai-fg-3)',
                }}
              >
                {grp.group}
              </div>
              <For each={grp.items}>
                {(item) => <SidebarItem item={item} active={loc.pathname === item.path} />}
              </For>
            </div>
          )}
        </For>
      </div>

      {/* per-volume telemetry from the rust list_volumes command */}
      <DiskFooter volume={primary()} loading={volumes.loading && !volumes()} error={volumes.error} />
    </div>
  );
}

function DiskFooter(props: { volume: Volume | null; loading: boolean; error: unknown }) {
  const pct = () => {
    const v = props.volume;
    if (!v || v.totalBytes === 0) return 0;
    // clamp, a bad read shouldn't overflow the bar
    return Math.max(0, Math.min(100, (v.usedBytes / v.totalBytes) * 100));
  };
  return (
    <div style={{ padding: '10px 12px 0', 'border-top': '1px solid var(--safai-line)' }}>
      <Show
        when={!props.error}
        fallback={
          <div style={{ 'font-size': '11px', color: 'var(--safai-fg-3)' }}>Disk unavailable</div>
        }
      >
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-2)',
            'margin-bottom': '6px',
            display: 'flex',
            'justify-content': 'space-between',
            gap: '8px',
          }}
        >
          <span
            style={{
              'white-space': 'nowrap',
              overflow: 'hidden',
              'text-overflow': 'ellipsis',
            }}
            title={props.volume?.mountPoint ?? ''}
          >
            {props.loading ? '-' : props.volume?.name ?? 'No disk'}
          </span>
          <span class="num" style={{ 'flex-shrink': 0 }}>
            {props.volume ? `${formatBytes(props.volume.freeBytes)} free` : ''}
          </span>
        </div>
        <div class="safai-bar">
          <div
            class="safai-bar__fill"
            style={{
              width: `${pct()}%`,
              background: 'linear-gradient(90deg, var(--safai-cyan), var(--safai-lilac))',
              transition: 'width 240ms ease-out',
            }}
          />
        </div>
        <div
          class="num"
          style={{ 'font-size': '10px', color: 'var(--safai-fg-3)', 'margin-top': '4px' }}
        >
          {props.volume
            ? `${formatBytes(props.volume.usedBytes)} / ${formatBytes(props.volume.totalBytes)} used`
            : ' '}
        </div>
      </Show>
    </div>
  );
}

function SidebarItem(props: { item: NavItem; active: boolean }) {
  return (
    <A
      href={props.item.path}
      style={{
        height: '30px',
        display: 'flex',
        'align-items': 'center',
        gap: '10px',
        padding: '0 10px',
        'border-radius': '8px',
        'font-size': '13px',
        color: props.active ? 'var(--safai-fg-0)' : 'var(--safai-fg-1)',
        background: props.active ? 'oklch(0.82 0.14 200 / 0.10)' : 'transparent',
        border: props.active ? '1px solid oklch(0.82 0.14 200 / 0.25)' : '1px solid transparent',
        'font-weight': props.active ? 500 : 400,
        cursor: 'pointer',
        'margin-bottom': '1px',
        'text-decoration': 'none',
      }}
    >
      <Icon
        name={props.item.icon}
        size={14}
        color={props.active ? 'var(--safai-cyan)' : 'var(--safai-fg-2)'}
      />
      <span style={{ flex: 1 }}>{props.item.label}</span>
      <Show when={props.item.count}>
        <span class="num" style={{ 'font-size': '10px', color: 'var(--safai-fg-3)' }}>
          {props.item.count}
        </span>
      </Show>
    </A>
  );
}
