import { JSX, Show } from 'solid-js';
import { OSChip, type OS } from './OSChip';

interface SafaiWindowProps {
  children: JSX.Element;
  os?: OS;
  title?: string;
  showOSChip?: boolean;
}

// window chrome. traffic-lights left, os chip right. titlebar drags via tauri.
export function SafaiWindow(props: SafaiWindowProps) {
  return (
    <div class="safai-root" style={{ display: 'flex', 'flex-direction': 'column' }}>
      <div
        data-tauri-drag-region
        style={{
          height: '40px',
          'flex-shrink': 0,
          display: 'flex',
          'align-items': 'center',
          padding: '0 14px',
          gap: '12px',
          background: 'oklch(0.14 0.008 240)',
          'border-bottom': '1px solid var(--safai-line)',
        }}
      >
        <div style={{ display: 'flex', gap: '8px', 'align-items': 'center' }}>
          <div style={{ width: '12px', height: '12px', 'border-radius': '50%', background: 'oklch(0.70 0.17 25)' }} />
          <div style={{ width: '12px', height: '12px', 'border-radius': '50%', background: 'oklch(0.82 0.14 80)' }} />
          <div style={{ width: '12px', height: '12px', 'border-radius': '50%', background: 'oklch(0.78 0.14 140)' }} />
        </div>
        <div
          style={{
            flex: 1,
            'text-align': 'center',
            'font-size': '12px',
            color: 'var(--safai-fg-2)',
            'letter-spacing': '0.02em',
          }}
        >
          {props.title ?? 'Safai'}
        </div>
        <Show when={(props.showOSChip ?? true) && props.os}>
          <OSChip os={props.os!} />
        </Show>
      </div>
      <div style={{ flex: 1, display: 'flex', 'min-height': 0 }}>{props.children}</div>
    </div>
  );
}
