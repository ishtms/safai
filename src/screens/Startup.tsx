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
import { Icon, type IconName } from '../components/Icon';
import {
  BOOT_SECONDS_PER_IMPACT,
  estimateBootSeconds,
  groupBySource,
  isToggleable,
  labelForSource,
  startupScan,
  startupToggle,
  type StartupImpact,
  type StartupItem,
  type StartupReport,
  type StartupSource,
} from '../lib/startup';
import { formatCount, formatRelativeTime } from '../lib/format';

// startup items manager. login-time autostarts grouped by source w/ a
// toggle each. hero shows before/after boot-time from per-item impact
// tiers. flipping a switch live-previews "after" before the toggle
// commits, so users can audit.
export default function Startup() {
  const [report, { refetch }] = createResource(startupScan);

  const [clock, setClock] = createSignal(Date.now());
  const tick = setInterval(() => setClock(Date.now()), 500);
  onCleanup(() => clearInterval(tick));

  // optimistic overrides, set on toggle + cleared when refetch reflects the
  // new state. lets "after" update instantly
  const [overrides, setOverrides] = createSignal<Map<string, boolean>>(new Map());

  // in-flight per item so the switch disables during a toggle
  const [pendingIds, setPendingIds] = createSignal<Set<string>>(new Set());

  const [status, setStatus] = createSignal<
    { tone: 'ok' | 'error'; text: string } | null
  >(null);

  // clear overrides on fresh scan
  let lastStamp: number | null = null;
  createEffect(() => {
    const r = report();
    if (!r) return;
    if (r.scannedAt === lastStamp) return;
    lastStamp = r.scannedAt;
    setOverrides(new Map());
  });

  const grouped = createMemo(() => {
    const r = report();
    return r ? groupBySource(r) : [];
  });

  const bootEstimate = createMemo(() => {
    const r = report();
    if (!r) return { before: 0, after: 0 };
    return estimateBootSeconds(r, overrides());
  });

  const enabledCount = createMemo(() => {
    const r = report();
    if (!r) return 0;
    const o = overrides();
    return r.items.filter((i) => (o.has(i.id) ? o.get(i.id) : i.enabled)).length;
  });

  const onToggle = async (item: StartupItem) => {
    if (!isToggleable(item.source)) return;
    if (pendingIds().has(item.id)) return;

    const o = overrides();
    const currentEffective = o.has(item.id) ? o.get(item.id)! : item.enabled;
    const next = !currentEffective;

    // optimistic so ui + estimate reflect instantly
    const newOverrides = new Map(o);
    newOverrides.set(item.id, next);
    setOverrides(newOverrides);

    const newPending = new Set(pendingIds());
    newPending.add(item.id);
    setPendingIds(newPending);

    try {
      await startupToggle(item.source, item.path, next);
      setStatus({
        tone: 'ok',
        text: `${item.name} ${next ? 'enabled' : 'disabled'}.`,
      });
    } catch (err) {
      // roll back the optimistic change
      const rolled = new Map(overrides());
      rolled.delete(item.id);
      setOverrides(rolled);
      setStatus({ tone: 'error', text: `Couldn't toggle ${item.name}: ${String(err)}` });
    } finally {
      const settled = new Set(pendingIds());
      settled.delete(item.id);
      setPendingIds(settled);
      // refetch to pick up side effects, effect above clears overrides
      // once the new scan lands
      refetch();
    }
  };

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Maintenance"
        title="Startup Items"
        subtitle="Audit and disable apps that launch at login."
        right={
          <div style={{ display: 'flex', gap: '8px', 'align-items': 'center' }}>
            <span style={{ 'font-size': '12px', color: 'var(--safai-fg-2)' }}>
              {formatCount(enabledCount())} enabled ·{' '}
              <span class="num" style={{ color: 'var(--safai-fg-0)', 'font-weight': 500 }}>
                ~{bootEstimate().after.toFixed(1)}s
              </span>{' '}
              boot
            </span>
            <button
              class="safai-btn safai-btn--ghost"
              onClick={() => refetch()}
              disabled={report.loading}
              aria-busy={report.loading}
            >
              <span class={report.loading ? 'safai-spin' : ''} style={{ display: 'inline-flex' }}>
                <Icon name="refresh" size={12} />
              </span>{' '}
              {report.loading ? 'Scanning…' : 'Rescan'}
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
            <BootHero
              before={bootEstimate().before}
              after={bootEstimate().after}
              loading={report.loading && !report()}
            />

            <Show
              when={!report.loading || report()}
              fallback={<For each={Array.from({ length: 4 })}>{() => <SkeletonRow />}</For>}
            >
              <For each={grouped()}>
                {(group) => (
                  <SourceGroup
                    source={group.source}
                    items={group.items}
                    overrides={overrides()}
                    pendingIds={pendingIds()}
                    onToggle={onToggle}
                  />
                )}
              </For>
              <Show when={(report()?.items ?? []).length === 0}>
                <EmptyState />
              </Show>
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
                  <span>Last scanned {formatRelativeTime(r().scannedAt, clock())}</span>
                  <span>·</span>
                  <span>Scanned in {Math.max(1, Math.round(r().durationMs))} ms</span>
                  <span>·</span>
                  <span>{formatCount(r().items.length)} items catalogued</span>
                  <span>·</span>
                  <span>Platform: {r().platform}</span>
                </div>
              )}
            </Show>
          </Show>
        </div>

        <SidePane
          report={report()}
          before={bootEstimate().before}
          after={bootEstimate().after}
        />
      </div>

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

// before/after boot-time. bars are flex rows scaled so the longer one
// fills the width. "after" updates live.
function BootHero(props: { before: number; after: number; loading: boolean }) {
  const max = () => Math.max(props.before, props.after, 1);
  const beforePct = () => (props.before / max()) * 100;
  const afterPct = () => (props.after / max()) * 100;
  const saved = () => Math.max(0, props.before - props.after);
  const mood = () => (saved() > 1 ? 'zoom' : 'sleepy');

  return (
    <div
      class="safai-card"
      style={{
        padding: '20px 24px',
        'margin-bottom': '16px',
        display: 'flex',
        'align-items': 'center',
        gap: '24px',
      }}
    >
      <Suds size={56} mood={mood()} float />
      <div style={{ flex: 1 }}>
        <div style={{ 'font-size': '12px', color: 'var(--safai-fg-3)', 'margin-bottom': '6px' }}>
          Estimated boot time
        </div>
        <div style={{ display: 'flex', 'align-items': 'center', gap: '12px' }}>
          <span
            class="num"
            style={{
              'font-size': '28px',
              'font-family': 'var(--safai-font-display)',
              'font-weight': 600,
              color: 'var(--safai-fg-0)',
              'font-variant-numeric': 'tabular-nums',
            }}
          >
            {props.loading ? '—' : `${props.after.toFixed(1)}s`}
          </span>
          <Show when={saved() > 0.1 && !props.loading}>
            <span
              class="safai-pill"
              style={{
                background: 'var(--safai-cyan-dim)',
                color: 'var(--safai-cyan)',
                'font-size': '11px',
              }}
            >
              −{saved().toFixed(1)}s saved
            </span>
          </Show>
        </div>
        <div style={{ 'margin-top': '12px', display: 'flex', 'flex-direction': 'column', gap: '6px' }}>
          <Bar label="Before" value={props.before} pct={beforePct()} dim />
          <Bar label="After" value={props.after} pct={afterPct()} dim={false} />
        </div>
      </div>
    </div>
  );
}

function Bar(props: { label: string; value: number; pct: number; dim: boolean }) {
  return (
    <div style={{ display: 'flex', 'align-items': 'center', gap: '10px' }}>
      <span
        style={{
          width: '50px',
          'font-size': '10px',
          color: 'var(--safai-fg-3)',
          'text-transform': 'uppercase',
          'letter-spacing': '0.12em',
        }}
      >
        {props.label}
      </span>
      <div
        style={{
          flex: 1,
          height: '8px',
          'border-radius': '4px',
          background: 'var(--safai-bg-2)',
          overflow: 'hidden',
        }}
      >
        <div
          style={{
            height: '100%',
            width: `${Math.min(100, Math.max(0, props.pct))}%`,
            background: props.dim ? 'var(--safai-fg-3)' : 'var(--safai-cyan)',
            transition: 'width 180ms ease-out',
          }}
        />
      </div>
      <span
        class="num"
        style={{ width: '52px', 'text-align': 'right', 'font-size': '12px', color: 'var(--safai-fg-1)' }}
      >
        {props.value.toFixed(1)}s
      </span>
    </div>
  );
}

function SourceGroup(props: {
  source: StartupSource;
  items: StartupItem[];
  overrides: Map<string, boolean>;
  pendingIds: Set<string>;
  onToggle: (item: StartupItem) => void;
}) {
  return (
    <div class="safai-card" style={{ 'margin-bottom': '12px', overflow: 'hidden' }}>
      <div
        style={{
          padding: '10px 16px',
          'border-bottom': '1px solid var(--safai-line)',
          display: 'flex',
          'align-items': 'center',
          gap: '10px',
          'font-size': '11px',
          color: 'var(--safai-fg-3)',
          'letter-spacing': '0.12em',
          'text-transform': 'uppercase',
        }}
      >
        <span>{labelForSource(props.source)}</span>
        <span style={{ color: 'var(--safai-fg-2)' }}>· {props.items.length}</span>
      </div>
      <For each={props.items}>
        {(item) => (
          <ItemRow
            item={item}
            overrides={props.overrides}
            isPending={props.pendingIds.has(item.id)}
            onToggle={() => props.onToggle(item)}
          />
        )}
      </For>
    </div>
  );
}

function ItemRow(props: {
  item: StartupItem;
  overrides: Map<string, boolean>;
  isPending: boolean;
  onToggle: () => void;
}) {
  const effective = () => {
    const o = props.overrides;
    if (o.has(props.item.id)) return o.get(props.item.id)!;
    return props.item.enabled;
  };
  const canToggle = () => isToggleable(props.item.source);
  const impactColour = () => impactAccent(props.item.impact);

  return (
    <div
      style={{
        padding: '14px 16px',
        display: 'flex',
        'align-items': 'center',
        gap: '12px',
        'border-bottom': '1px solid var(--safai-line)',
      }}
    >
      <div
        style={{
          width: '28px',
          height: '28px',
          'border-radius': '7px',
          background: `color-mix(in oklab, ${impactColour()} 16%, transparent)`,
          display: 'flex',
          'align-items': 'center',
          'justify-content': 'center',
          'flex-shrink': 0,
        }}
      >
        <Icon name={props.item.icon as IconName} size={13} color={impactColour()} />
      </div>
      <div style={{ flex: 1, 'min-width': 0 }}>
        <div
          style={{
            display: 'flex',
            'align-items': 'center',
            gap: '8px',
            'margin-bottom': '3px',
          }}
        >
          <span
            style={{
              'font-size': '13px',
              'font-weight': 500,
              color: 'var(--safai-fg-0)',
              'white-space': 'nowrap',
              overflow: 'hidden',
              'text-overflow': 'ellipsis',
            }}
          >
            {props.item.name}
          </span>
          <span
            class="safai-pill"
            style={{
              background: `color-mix(in oklab, ${impactColour()} 18%, transparent)`,
              color: impactColour(),
              'font-size': '9px',
            }}
            title={`${BOOT_SECONDS_PER_IMPACT[props.item.impact].toFixed(1)}s boot impact`}
          >
            {props.item.impact}
          </span>
          <Show when={!props.item.isUser}>
            <span
              class="safai-pill"
              style={{
                background: 'var(--safai-bg-2)',
                color: 'var(--safai-fg-3)',
                'font-size': '9px',
              }}
            >
              system
            </span>
          </Show>
        </div>
        <div
          class="mono"
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-3)',
            'white-space': 'nowrap',
            overflow: 'hidden',
            'text-overflow': 'ellipsis',
          }}
          title={props.item.command || props.item.path}
        >
          {props.item.command || props.item.path}
        </div>
      </div>
      <Toggle
        checked={effective()}
        disabled={!canToggle() || props.isPending}
        pending={props.isPending}
        onToggle={props.onToggle}
        title={canToggle() ? '' : 'Read-only — requires admin privileges'}
      />
    </div>
  );
}

function Toggle(props: {
  checked: boolean;
  disabled: boolean;
  pending: boolean;
  onToggle: () => void;
  title?: string;
}) {
  return (
    <button
      role="switch"
      aria-checked={props.checked}
      aria-busy={props.pending}
      disabled={props.disabled}
      onClick={(e) => {
        e.stopPropagation();
        if (!props.disabled) props.onToggle();
      }}
      title={props.title}
      style={{
        width: '42px',
        height: '24px',
        'border-radius': '12px',
        border: '1px solid var(--safai-line)',
        background: props.checked ? 'var(--safai-cyan)' : 'var(--safai-bg-2)',
        position: 'relative',
        cursor: props.disabled ? 'not-allowed' : 'pointer',
        opacity: props.disabled ? 0.5 : 1,
        transition: 'background 160ms ease-out',
        padding: 0,
        'flex-shrink': 0,
      }}
    >
      <span
        style={{
          position: 'absolute',
          top: '2px',
          left: props.checked ? '20px' : '2px',
          width: '18px',
          height: '18px',
          'border-radius': '9px',
          background: 'oklch(0.98 0.02 240)',
          transition: 'left 160ms ease-out',
          'box-shadow': '0 1px 2px oklch(0 0 0 / 0.2)',
        }}
      />
    </button>
  );
}

function impactAccent(impact: StartupImpact): string {
  switch (impact) {
    case 'high':
      return 'var(--safai-coral)';
    case 'medium':
      return 'var(--safai-amber, var(--safai-fg-1))';
    case 'low':
      return 'var(--safai-cyan)';
  }
}

function SidePane(props: {
  report: StartupReport | undefined;
  before: number;
  after: number;
}) {
  const heavyCount = () =>
    props.report?.items.filter((i) => i.enabled && i.impact === 'high').length ?? 0;
  const note = () => {
    if (!props.report) return 'Loading your login agenda…';
    if (props.report.items.length === 0) {
      return 'Nothing launches at login on this machine. Boot will be snappy either way.';
    }
    if (heavyCount() > 0) {
      return `${heavyCount()} heavy app${heavyCount() === 1 ? '' : 's'} launches at login. Flipping them off cuts ~${(
        heavyCount() * BOOT_SECONDS_PER_IMPACT.high
      ).toFixed(1)}s from the boot budget.`;
    }
    return 'Looks tidy — only lightweight helpers launch at login.';
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
          <Suds size={36} mood={heavyCount() > 2 ? 'shocked' : 'happy'} />
          <div style={{ 'font-size': '12px', color: 'var(--safai-fg-1)', 'font-weight': 500 }}>
            Suds says
          </div>
        </div>
        <div style={{ 'font-size': '13px', color: 'var(--safai-fg-1)', 'line-height': 1.5 }}>
          {note()}
        </div>
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-3)',
            'margin-top': '12px',
            'padding-top': '12px',
            'border-top': '1px solid var(--safai-line)',
          }}
        >
          Toggles are reversible — flip an item back on any time. Nothing is deleted.
        </div>
      </div>

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
          Impact legend
        </div>
        <LegendRow impact="high" />
        <LegendRow impact="medium" />
        <LegendRow impact="low" />
      </div>
    </div>
  );
}

function LegendRow(props: { impact: StartupImpact }) {
  const c = impactAccent(props.impact);
  const secs = BOOT_SECONDS_PER_IMPACT[props.impact];
  return (
    <div
      style={{
        display: 'flex',
        'align-items': 'center',
        gap: '10px',
        padding: '6px 0',
        'font-size': '12px',
        color: 'var(--safai-fg-2)',
      }}
    >
      <span
        style={{
          width: '12px',
          height: '12px',
          'border-radius': '3px',
          background: c,
        }}
      />
      <span style={{ flex: 1, 'text-transform': 'capitalize' }}>{props.impact}</span>
      <span class="num" style={{ color: 'var(--safai-fg-3)' }}>
        ~{secs.toFixed(1)}s/item
      </span>
    </div>
  );
}

function EmptyState() {
  return (
    <div
      class="safai-card"
      style={{
        padding: '40px 28px',
        display: 'flex',
        'align-items': 'center',
        gap: '20px',
      }}
    >
      <Suds size={64} mood="sleepy" />
      <div>
        <div style={{ 'font-size': '14px', color: 'var(--safai-fg-0)', 'margin-bottom': '4px' }}>
          Nothing launches at login
        </div>
        <div style={{ 'font-size': '12px', color: 'var(--safai-fg-2)' }}>
          No autostart entries, launch agents, or startup-folder items were found.
        </div>
      </div>
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
          Couldn't scan startup items
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
