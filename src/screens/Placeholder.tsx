import { useLocation } from '@solidjs/router';
import { findNavItem } from '../lib/nav';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon } from '../components/Icon';

// stub, each is swapped in by the real screen
export function Placeholder() {
  const loc = useLocation();
  const found = () => findNavItem(loc.pathname);

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb={found()?.group ?? 'Safai'}
        title={found()?.item.label ?? 'Coming soon'}
        subtitle="This screen ships in a later release. The shell + design system are live."
        right={
          <button class="safai-btn safai-btn--ghost" disabled>
            <Icon name="clock" size={12} /> Coming soon
          </button>
        }
      />
      <div
        style={{
          flex: 1,
          overflow: 'auto',
          padding: '40px 28px',
          display: 'flex',
          'align-items': 'center',
          'justify-content': 'center',
        }}
      >
        <div
          class="safai-card safai-sheen"
          style={{
            padding: '40px 48px',
            display: 'flex',
            'align-items': 'center',
            gap: '28px',
            'max-width': '720px',
            background: 'linear-gradient(135deg, oklch(0.22 0.02 240), oklch(0.20 0.02 260))',
            border: '1px solid oklch(0.82 0.14 200 / 0.18)',
          }}
        >
          <Suds size={88} mood="sleepy" float />
          <div>
            <div
              style={{
                'font-size': '11px',
                color: 'var(--safai-fg-2)',
                'letter-spacing': '0.15em',
                'text-transform': 'uppercase',
                'margin-bottom': '8px',
              }}
            >
              Scaffold
            </div>
            <h2
              style={{
                'font-size': '28px',
                'letter-spacing': '-0.025em',
                'margin-bottom': '8px',
              }}
            >
              Suds is napping on this one
            </h2>
            <div style={{ 'font-size': '14px', color: 'var(--safai-fg-1)', 'line-height': 1.6 }}>
              The design system, shell, sidebar, and routing are live. Each sidebar entry is wired
              up. The actual screen content lands in its dedicated module - see <code class="mono">roadmap.md</code>.
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
