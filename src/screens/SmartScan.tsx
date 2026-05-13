import { createResource, createSignal, For, Show, onCleanup, onMount } from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon, type IconName } from '../components/Icon';
import { CountUp } from '../components/CountUp';
import { fetchSmartScanSummary, type CategorySummary } from '../lib/scanner';
import { listVolumes, pickPrimary, type Volume } from '../lib/volumes';
import { formatBytes, formatCount, formatRelativeTime, splitBytes } from '../lib/format';

// smart scan dashboard. numbers from smart_scan_summary (rust reads
// LastScanStore). no scan yet in this process = hero reads "Never" and
// numbers are 0, user hits Rescan.
export default function SmartScan() {
  const [summary, { refetch }] = createResource(fetchSmartScanSummary);
  const [volumes] = createResource(listVolumes);
  const navigate = useNavigate();

  // formatRelativeTime is pure on now(). 30s tick so "2 min ago" rolls
  // forward without interaction
  const [clock, setClock] = createSignal(Date.now());
  onMount(() => {
    const tick = window.setInterval(() => setClock(Date.now()), 30_000);
    onCleanup(() => window.clearInterval(tick));
  });

  const totalBytes = () => summary()?.totalBytes ?? 0;
  const totalItems = () => summary()?.totalItems ?? 0;
  const primaryVolume = () => (volumes() ? pickPrimary(volumes()!) : null);

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Overview"
        title="Smart Scan"
        subtitle="One-click tidy. Suds checks everything and tells you what can go."
        right={
          <div style={{ display: 'flex', gap: '8px' }}>
            <button
              class="safai-btn safai-btn--ghost"
              onClick={() => {
                // refetch on return, scanning screen owns its own lifecycle
                refetch();
                navigate('/scanning');
              }}
              disabled={summary.loading}
              aria-label="Rescan"
            >
              <Icon name="refresh" size={12} /> Rescan
            </button>
          </div>
        }
      />

      <div style={{ flex: 1, overflow: 'auto', padding: '24px 28px 40px' }}>
        <Show when={summary.error}>
          <ErrorCard
            message={String(summary.error)}
            onRetry={() => refetch()}
          />
        </Show>

        <Show when={!summary.error}>
          <HeroCard
            totalBytes={totalBytes()}
            totalItems={totalItems()}
            scannedAt={summary()?.scannedAt ?? null}
            now={clock()}
            loading={summary.loading && !summary()}
            primary={primaryVolume()}
            onReview={() => navigate('/junk')}
          />

          <Show when={(summary()?.categories ?? []).length > 0}>
            <SectionLabel
              left="What's taking up space"
              right="Tap a category to dig in"
            />
            <StackedBar categories={summary()?.categories ?? []} total={totalBytes()} />
            <CategoryGrid
              categories={summary()?.categories ?? []}
              loading={summary.loading && !summary()}
            />
          </Show>
        </Show>
      </div>
    </div>
  );
}

// subcomponents

interface HeroProps {
  totalBytes: number;
  totalItems: number;
  scannedAt: number | null;
  now: number;
  loading: boolean;
  primary: Volume | null;
  onReview: () => void;
}

function HeroCard(props: HeroProps) {
  const split = () => splitBytes(props.totalBytes);
  const hasResults = () => props.totalBytes > 0 && props.scannedAt != null;
  const neverScanned = () => props.scannedAt == null && !props.loading;
  const blurb = () => {
    if (props.loading) return '';
    if (neverScanned()) {
      return "Haven't taken a look yet. Hit Rescan and I'll give the place a once-over.";
    }
    if (!hasResults()) {
      return "All tidy - nothing worth flagging right now. Rescan any time.";
    }
    return "Found some stuff worth a second look. Open the categories below or hit Review & clean.";
  };
  return (
    <div
      class="safai-card safai-sheen"
      style={{
        padding: '28px 32px',
        display: 'flex',
        'align-items': 'center',
        gap: '32px',
        'margin-bottom': '20px',
        background: 'linear-gradient(135deg, oklch(0.22 0.02 240), oklch(0.20 0.02 260))',
        border: '1px solid oklch(0.82 0.14 200 / 0.25)',
      }}
    >
      <Suds size={96} mood={props.totalBytes > 0 ? 'happy' : 'sleepy'} float />
      <div style={{ flex: 1 }}>
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-2)',
            'letter-spacing': '0.15em',
            'text-transform': 'uppercase',
            'margin-bottom': '6px',
          }}
        >
          Last scan · {formatRelativeTime(props.scannedAt, props.now)}
        </div>
        <div
          style={{
            'font-size': '14px',
            color: 'var(--safai-fg-1)',
            'margin-bottom': '4px',
            display: 'flex',
            'align-items': 'baseline',
            gap: '10px',
          }}
        >
          <span>You can get back</span>
          <Show when={props.primary}>
            {(vol) => (
              <span
                class="num"
                style={{ 'font-size': '11px', color: 'var(--safai-fg-3)' }}
                title={`${vol().name} · ${vol().mountPoint}`}
              >
                · {formatBytes(vol().freeBytes)} free on {vol().name}
              </span>
            )}
          </Show>
        </div>
        <div style={{ display: 'flex', 'align-items': 'baseline', gap: '10px' }}>
          <div
            class="num"
            style={{
              'font-size': '56px',
              'font-weight': 600,
              'font-family': 'var(--safai-font-display)',
              'letter-spacing': '-0.04em',
              'line-height': 1,
              color: 'var(--safai-cyan)',
              'font-variant-numeric': 'tabular-nums',
            }}
          >
            {props.loading ? '-' : split().value}
          </div>
          <div style={{ 'font-size': '20px', color: 'var(--safai-fg-1)', 'font-weight': 500 }}>
            {split().unit}
          </div>
          <div style={{ 'font-size': '13px', color: 'var(--safai-fg-2)', 'margin-left': '12px' }}>
            across{' '}
            <CountUp
              value={props.totalItems}
              format={(v) => formatCount(Math.round(v))}
            />{' '}
            items
          </div>
        </div>
        <div
          style={{
            'font-size': '13px',
            color: 'var(--safai-fg-1)',
            'margin-top': '10px',
            'max-width': '520px',
          }}
        >
          {blurb()}
        </div>
      </div>
      <div
        style={{
          display: 'flex',
          'flex-direction': 'column',
          gap: '8px',
          'align-self': 'stretch',
          'justify-content': 'center',
        }}
      >
        <button
          class="safai-btn safai-btn--primary safai-btn--big"
          style={{ 'min-width': '180px' }}
          disabled={!hasResults()}
          onClick={props.onReview}
        >
          Review & clean
        </button>
      </div>
    </div>
  );
}

function SectionLabel(props: { left: string; right: string }) {
  return (
    <div
      style={{
        'margin-bottom': '10px',
        display: 'flex',
        'justify-content': 'space-between',
        'align-items': 'center',
      }}
    >
      <div
        style={{
          'font-size': '12px',
          color: 'var(--safai-fg-2)',
          'letter-spacing': '0.08em',
          'text-transform': 'uppercase',
        }}
      >
        {props.left}
      </div>
      <div style={{ 'font-size': '11px', color: 'var(--safai-fg-3)' }}>{props.right}</div>
    </div>
  );
}

function StackedBar(props: { categories: CategorySummary[]; total: number }) {
  return (
    <div
      role="img"
      aria-label={`${props.categories.length} categories, ${formatBytes(props.total)} total`}
      style={{
        height: '10px',
        display: 'flex',
        'border-radius': '5px',
        overflow: 'hidden',
        'margin-bottom': '20px',
        border: '1px solid var(--safai-line)',
        background: 'var(--safai-bg-2)',
      }}
    >
      <For each={props.categories}>
        {(c) => (
          <div
            // flex-grow = bytes, widths sum to 100%
            style={{
              'flex-grow': Math.max(1, c.bytes), // max(1, ..) so tiny slices still show
              'flex-basis': 0,
              background: `var(${c.colorVar})`,
            }}
            title={`${c.label} - ${formatBytes(c.bytes)}`}
          />
        )}
      </For>
    </div>
  );
}

function CategoryGrid(props: { categories: CategorySummary[]; loading: boolean }) {
  return (
    <div
      style={{
        display: 'grid',
        'grid-template-columns': 'repeat(3, 1fr)',
        gap: '12px',
      }}
    >
      <Show
        when={!props.loading}
        fallback={
          <For each={Array.from({ length: 6 })}>
            {() => <SkeletonCard />}
          </For>
        }
      >
        <For each={props.categories}>{(c) => <CategoryCard cat={c} />}</For>
      </Show>
    </div>
  );
}

function CategoryCard(props: { cat: CategorySummary }) {
  const color = `var(${props.cat.colorVar})`;
  return (
    <div
      class="safai-card safai-card--hover"
      style={{ padding: '18px', cursor: 'pointer' }}
    >
      <div style={{ display: 'flex', 'align-items': 'flex-start', 'margin-bottom': '14px' }}>
        <div
          style={{
            width: '32px',
            height: '32px',
            'border-radius': '8px',
            background: `color-mix(in oklab, ${color} 15%, transparent)`,
            display: 'flex',
            'align-items': 'center',
            'justify-content': 'center',
          }}
        >
          <Icon name={props.cat.icon as IconName} size={15} color={color} />
        </div>
        <div style={{ flex: 1 }} />
        <div class="safai-check safai-check--on">
          <Icon name="check" size={9} color="oklch(0.18 0.02 240)" strokeWidth={2.2} />
        </div>
      </div>
      <div style={{ 'font-size': '13px', color: 'var(--safai-fg-1)', 'margin-bottom': '2px' }}>
        {props.cat.label}
      </div>
      <div
        class="num"
        style={{
          'font-size': '26px',
          'font-family': 'var(--safai-font-display)',
          'font-weight': 600,
          'letter-spacing': '-0.03em',
          'margin-bottom': '8px',
          'font-variant-numeric': 'tabular-nums',
        }}
      >
        {formatBytes(props.cat.bytes)}
      </div>
      <div style={{ 'font-size': '11px', color: 'var(--safai-fg-3)' }}>{props.cat.safeNote}</div>
    </div>
  );
}

function SkeletonCard() {
  return (
    <div
      class="safai-card"
      style={{
        padding: '18px',
        height: '118px',
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
      <Suds size={72} mood="shocked" />
      <div style={{ flex: 1 }}>
        <div style={{ 'font-size': '14px', color: 'var(--safai-fg-0)', 'margin-bottom': '4px' }}>
          Couldn't load scan summary
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
