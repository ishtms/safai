import { JSX, Show } from 'solid-js';

interface SafaiToolbarProps {
  title: string;
  subtitle?: string;
  breadcrumb?: string;
  right?: JSX.Element;
}

export function SafaiToolbar(props: SafaiToolbarProps) {
  return (
    <div
      style={{
        padding: '22px 28px 18px',
        display: 'flex',
        'align-items': 'flex-end',
        gap: '16px',
        'border-bottom': '1px solid var(--safai-line)',
        background: 'var(--safai-bg-0)',
        'flex-shrink': 0,
      }}
    >
      <div style={{ flex: 1, 'min-width': 0 }}>
        <Show when={props.breadcrumb}>
          <div
            style={{
              'font-size': '11px',
              color: 'var(--safai-fg-3)',
              'letter-spacing': '0.08em',
              'text-transform': 'uppercase',
              'margin-bottom': '4px',
            }}
          >
            {props.breadcrumb}
          </div>
        </Show>
        <h2 style={{ 'font-size': '24px', 'letter-spacing': '-0.025em' }}>{props.title}</h2>
        <Show when={props.subtitle}>
          <div style={{ 'font-size': '13px', color: 'var(--safai-fg-1)', 'margin-top': '4px' }}>
            {props.subtitle}
          </div>
        </Show>
      </div>
      {props.right}
    </div>
  );
}
