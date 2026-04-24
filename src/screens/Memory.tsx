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
import { formatBytes, splitBytes } from '../lib/format';
import {
  pressureColour,
  pressureTone,
  type ActivitySnapshot,
  type MemorySnapshot,
  type ProcessRow,
} from '../lib/activity';
import {
  acquireActivityStream,
  latestSnapshot,
  releaseActivityStream,
  snapshotHistory,
} from '../lib/activityStream';

// memory screen. pressure ring + used/free/swap hero, top hogs in sidebar,
// 60-tick rolling sparkline of used memory. all driven off one
// activity://snapshot subscription held for the screen's lifetime.

const SPARK_HISTORY = 60;

export default function Memory(): JSX.Element {
  const snap = latestSnapshot();
  const history = snapshotHistory();
  const [error] = createSignal<string | null>(null);

  onMount(() => {
    acquireActivityStream();
  });

  onCleanup(() => {
    releaseActivityStream();
  });

  const mem = createMemo(() => snap()?.memory);
  const pressure = createMemo(() => mem()?.pressurePercent ?? 0);
  const tone = createMemo(() => pressureTone(pressure()));
  const mood = createMemo<'happy' | 'sleepy' | 'shocked'>(() => {
    const t = tone();
    if (t === 'alert') return 'shocked';
    if (t === 'warn') return 'sleepy';
    return 'happy';
  });
  const topMem = createMemo<ProcessRow[]>(() => snap()?.topByMemory ?? []);

  return (
    <div style={{ flex: 1, display: 'flex', 'flex-direction': 'column', 'min-width': 0 }}>
      <SafaiToolbar
        breadcrumb="Maintenance"
        title="Memory"
        subtitle="Live RAM pressure, swap, and the biggest in-memory processes."
        right={
          <Show when={snap()}>
            {(s) => (
              <span style={{ 'font-size': '12px', color: 'var(--safai-fg-3)' }}>
                Tick #
                <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
                  {s().tick}
                </span>
              </span>
            )}
          </Show>
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
          <Show when={error()}>
            {(e) => (
              <div
                class="safai-card"
                style={{
                  padding: '14px 18px',
                  'margin-bottom': '16px',
                  color: 'var(--safai-coral)',
                  'font-size': '12px',
                }}
              >
                {e()}
              </div>
            )}
          </Show>

          <PressureHero
            snap={snap()}
            pressure={pressure()}
            tone={tone()}
            mood={mood()}
          />

          <UsageHistoryCard
            history={history()}
            colour={pressureColour(pressure())}
          />

          <CpuHistoryCard history={history()} />
        </div>

        <div>
          <div class="safai-card" style={{ padding: '18px', 'margin-bottom': '12px' }}>
            <div
              style={{
                'font-size': '11px',
                color: 'var(--safai-fg-3)',
                'letter-spacing': '0.12em',
                'text-transform': 'uppercase',
                'margin-bottom': '12px',
              }}
            >
              Top memory
            </div>
            <Show
              when={topMem().length > 0}
              fallback={
                <div style={{ 'font-size': '12px', color: 'var(--safai-fg-3)' }}>
                  Waiting for the first tick…
                </div>
              }
            >
              <For each={topMem()}>
                {(p) => (
                  <TopRow
                    name={p.name}
                    bytes={p.memoryBytes}
                    // share of USED memory, not total. a 1GB firefox on
                    // 16GB would look like 6% of total which under-reads it
                    share={mem()?.usedBytes ? p.memoryBytes / mem()!.usedBytes : 0}
                  />
                )}
              </For>
            </Show>
          </div>

          <SwapCard mem={mem()} />
        </div>
      </div>
    </div>
  );
}

// sub-components

function PressureHero(props: {
  snap: ActivitySnapshot | null;
  pressure: number;
  tone: 'ok' | 'warn' | 'alert';
  mood: 'happy' | 'sleepy' | 'shocked';
}) {
  const mem = () => props.snap?.memory;
  const used = () => mem()?.usedBytes ?? 0;
  const total = () => mem()?.totalBytes ?? 0;
  const free = () => Math.max(0, total() - used());
  const u = () => splitBytes(used());
  const f = () => splitBytes(free());
  const t = () => splitBytes(total());
  const accent = () => pressureColour(props.pressure);

  return (
    <div
      class="safai-card"
      style={{
        padding: '22px 26px',
        display: 'flex',
        'align-items': 'center',
        gap: '28px',
      }}
    >
      <Suds size={56} mood={props.mood} float />
      <PressureRing percent={props.pressure} colour={accent()} />
      <div style={{ flex: 1 }}>
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-3)',
            'letter-spacing': '0.12em',
            'text-transform': 'uppercase',
            'margin-bottom': '4px',
          }}
        >
          Memory pressure
        </div>
        <div style={{ display: 'flex', gap: '18px', 'align-items': 'baseline' }}>
          <div>
            <span
              class="num"
              style={{
                'font-size': '30px',
                'font-family': 'var(--safai-font-display)',
                'font-weight': 600,
                color: 'var(--safai-fg-0)',
                'font-variant-numeric': 'tabular-nums',
              }}
            >
              {props.pressure.toFixed(0)}
            </span>
            <span style={{ 'font-size': '14px', color: 'var(--safai-fg-3)', 'margin-left': '2px' }}>%</span>
          </div>
          <span
            class="safai-pill"
            style={{
              background: `color-mix(in oklab, ${accent()} 16%, transparent)`,
              color: accent(),
              'font-size': '11px',
              'text-transform': 'capitalize',
            }}
          >
            {props.tone}
          </span>
        </div>
        <div style={{ 'margin-top': '14px', display: 'flex', gap: '18px', 'font-size': '12px', color: 'var(--safai-fg-2)' }}>
          <Stat label="Used" split={u()} />
          <Stat label="Free" split={f()} />
          <Stat label="Total" split={t()} />
        </div>
      </div>
    </div>
  );
}

function Stat(props: { label: string; split: { value: string; unit: string } }) {
  return (
    <div>
      <span style={{ color: 'var(--safai-fg-3)' }}>{props.label} </span>
      <span class="num" style={{ color: 'var(--safai-fg-0)', 'font-weight': 500 }}>
        {props.split.value}
      </span>
      <span style={{ color: 'var(--safai-fg-3)' }}> {props.split.unit}</span>
    </div>
  );
}

function PressureRing(props: { percent: number; colour: string }) {
  const size = 96;
  const stroke = 10;
  const r = (size - stroke) / 2;
  const circumference = 2 * Math.PI * r;
  const offset = () => circumference * (1 - Math.min(100, Math.max(0, props.percent)) / 100);
  return (
    <svg
      width={size}
      height={size}
      role="img"
      aria-label={`${props.percent.toFixed(0)}% memory pressure`}
    >
      <circle
        cx={size / 2}
        cy={size / 2}
        r={r}
        fill="none"
        stroke="var(--safai-bg-2)"
        stroke-width={stroke}
      />
      <circle
        cx={size / 2}
        cy={size / 2}
        r={r}
        fill="none"
        stroke={props.colour}
        stroke-width={stroke}
        stroke-linecap="round"
        stroke-dasharray={String(circumference)}
        stroke-dashoffset={String(offset())}
        transform={`rotate(-90 ${size / 2} ${size / 2})`}
        style={{ transition: 'stroke-dashoffset 360ms ease-out' }}
      />
    </svg>
  );
}

// rolling used-memory chart. normalises against the observed range, not
// totalBytes, so a few hundred MB swing on 16 GiB shows as a legible curve
// instead of a flat line at ~50%. absolute totals live in separate readouts.
function UsageHistoryCard(props: {
  history: ActivitySnapshot[];
  colour: string;
}) {
  const values = () => props.history.map((h) => h.memory.usedBytes);
  const current = () => values()[values().length - 1] ?? 0;
  const peak = () => values().reduce((m, v) => Math.max(m, v), 0);
  const trough = () =>
    values().length === 0 ? 0 : values().reduce((m, v) => Math.min(m, v), values()[0]!);
  const total = () =>
    props.history[props.history.length - 1]?.memory.totalBytes ?? 0;
  const range = () => {
    // 5% pad so the curve never kisses the card edges. if every sample is
    // identical (frozen-memory test rig) use ±1 byte so the line still draws
    // mid-card
    const lo = trough();
    const hi = peak();
    if (hi === lo) return { lo: lo - 1, hi: hi + 1 };
    const pad = (hi - lo) * 0.05;
    return { lo: lo - pad, hi: hi + pad };
  };

  return (
    <div class="safai-card" style={{ padding: '20px 24px', 'margin-top': '16px' }}>
      <div
        style={{
          display: 'flex',
          'justify-content': 'space-between',
          'align-items': 'baseline',
          'margin-bottom': '6px',
        }}
      >
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-3)',
            'letter-spacing': '0.12em',
            'text-transform': 'uppercase',
          }}
        >
          Used memory · rolling {SPARK_HISTORY} ticks
        </div>
        <span style={{ 'font-size': '11px', color: 'var(--safai-fg-3)' }}>
          <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
            {values().length}
          </span>{' '}
          samples
        </span>
      </div>
      <div
        style={{
          display: 'flex',
          gap: '20px',
          'margin-bottom': '12px',
          'font-size': '12px',
        }}
      >
        <Readout label="Now" bytes={current()} prominent />
        <Readout label="Peak" bytes={peak()} />
        <Readout label="Min" bytes={trough()} />
        <Readout label="Total" bytes={total()} />
      </div>
      <SparklineWithAxis
        values={values()}
        lo={range().lo}
        hi={range().hi}
        colour={props.colour}
        formatAxis={(v) => formatBytes(v)}
      />
    </div>
  );
}

// second chart so the screen still shows something when memory's flat but
// cpu spikes. uses system-wide average %.
function CpuHistoryCard(props: { history: ActivitySnapshot[] }) {
  const values = () => props.history.map((h) => h.cpu.averagePercent);
  const current = () => values()[values().length - 1] ?? 0;
  const peak = () => values().reduce((m, v) => Math.max(m, v), 0);
  return (
    <div class="safai-card" style={{ padding: '20px 24px', 'margin-top': '16px' }}>
      <div
        style={{
          display: 'flex',
          'justify-content': 'space-between',
          'align-items': 'baseline',
          'margin-bottom': '6px',
        }}
      >
        <div
          style={{
            'font-size': '11px',
            color: 'var(--safai-fg-3)',
            'letter-spacing': '0.12em',
            'text-transform': 'uppercase',
          }}
        >
          CPU · rolling {SPARK_HISTORY} ticks
        </div>
        <span style={{ 'font-size': '11px', color: 'var(--safai-fg-3)' }}>
          Now{' '}
          <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
            {current().toFixed(0)}%
          </span>{' '}
          · Peak{' '}
          <span class="num" style={{ color: 'var(--safai-fg-1)' }}>
            {peak().toFixed(0)}%
          </span>
        </span>
      </div>
      <SparklineWithAxis
        values={values()}
        lo={0}
        hi={100}
        colour="var(--safai-cyan)"
        formatAxis={(v) => `${Math.round(v)}%`}
      />
    </div>
  );
}

function Readout(props: { label: string; bytes: number; prominent?: boolean }) {
  const split = () => splitBytes(props.bytes);
  return (
    <div>
      <div style={{ color: 'var(--safai-fg-3)', 'font-size': '10px', 'text-transform': 'uppercase', 'letter-spacing': '0.1em' }}>
        {props.label}
      </div>
      <div>
        <span
          class="num"
          style={{
            'font-size': props.prominent ? '16px' : '13px',
            'font-weight': props.prominent ? 600 : 500,
            color: 'var(--safai-fg-0)',
          }}
        >
          {split().value}
        </span>
        <span style={{ 'font-size': '11px', color: 'var(--safai-fg-3)', 'margin-left': '2px' }}>
          {split().unit}
        </span>
      </div>
    </div>
  );
}

// filled-area line chart w/ axis line + min/max labels on the right.
// caller picks the [lo, hi] band (memory: observed range, cpu: 0-100) so
// each curve fills its card.
function SparklineWithAxis(props: {
  values: number[];
  lo: number;
  hi: number;
  colour: string;
  formatAxis: (v: number) => string;
}) {
  const width = 600;
  const height = 96;
  const padTop = 6;
  const padBottom = 6;
  const plotH = height - padTop - padBottom;
  const yFor = (v: number) => {
    const span = Math.max(1, props.hi - props.lo);
    const t = (v - props.lo) / span;
    return padTop + (1 - Math.max(0, Math.min(1, t))) * plotH;
  };
  const line = () => {
    const vs = props.values;
    if (vs.length === 0) return '';
    const step = vs.length > 1 ? width / (vs.length - 1) : 0;
    return vs
      .map((v, i) => {
        const x = i * step;
        const y = yFor(v);
        return `${i === 0 ? 'M' : 'L'}${x.toFixed(2)},${y.toFixed(2)}`;
      })
      .join(' ');
  };
  const area = () => {
    const vs = props.values;
    if (vs.length === 0) return '';
    const step = vs.length > 1 ? width / (vs.length - 1) : 0;
    const pts = vs
      .map((v, i) => {
        const x = i * step;
        const y = yFor(v);
        return `${i === 0 ? 'M' : 'L'}${x.toFixed(2)},${y.toFixed(2)}`;
      })
      .join(' ');
    const lastX = ((vs.length - 1) * step).toFixed(2);
    const floor = (padTop + plotH).toFixed(2);
    return `${pts} L${lastX},${floor} L0,${floor} Z`;
  };

  return (
    <div style={{ display: 'flex', 'align-items': 'stretch', gap: '8px' }}>
      <svg
        viewBox={`0 0 ${width} ${height}`}
        preserveAspectRatio="none"
        style={{ flex: 1, height: `${height}px`, display: 'block' }}
      >
        <path d={area()} fill={props.colour} fill-opacity="0.12" />
        <line
          x1="0"
          x2={width}
          y1={padTop + plotH}
          y2={padTop + plotH}
          stroke="var(--safai-line)"
          stroke-width="1"
        />
        <path d={line()} fill="none" stroke={props.colour} stroke-width="2" />
      </svg>
      <div
        style={{
          display: 'flex',
          'flex-direction': 'column',
          'justify-content': 'space-between',
          'font-size': '10px',
          color: 'var(--safai-fg-3)',
          'font-variant-numeric': 'tabular-nums',
          'min-width': '56px',
          'padding-top': '4px',
          'padding-bottom': '4px',
          'text-align': 'right',
        }}
      >
        <span>{props.formatAxis(props.hi)}</span>
        <span>{props.formatAxis(props.lo)}</span>
      </div>
    </div>
  );
}

function TopRow(props: { name: string; bytes: number; share: number }) {
  return (
    <div
      style={{
        display: 'grid',
        'grid-template-columns': '1fr auto',
        gap: '6px 10px',
        'align-items': 'center',
        padding: '6px 0',
        'border-bottom': '1px solid var(--safai-line)',
      }}
    >
      <div
        style={{
          'font-size': '12px',
          color: 'var(--safai-fg-1)',
          'white-space': 'nowrap',
          overflow: 'hidden',
          'text-overflow': 'ellipsis',
        }}
        title={props.name}
      >
        {props.name}
      </div>
      <div class="num" style={{ 'font-size': '11px', color: 'var(--safai-fg-2)' }}>
        {formatBytes(props.bytes)}
      </div>
      <div
        style={{
          'grid-column': '1 / span 2',
          height: '4px',
          'border-radius': '2px',
          background: 'var(--safai-bg-2)',
          overflow: 'hidden',
        }}
      >
        <div
          style={{
            height: '100%',
            width: `${Math.min(100, Math.max(0, props.share * 100))}%`,
            background: 'var(--safai-cyan)',
            transition: 'width 360ms ease-out',
          }}
        />
      </div>
    </div>
  );
}

function SwapCard(props: { mem: MemorySnapshot | undefined }) {
  const total = () => props.mem?.swapTotalBytes ?? 0;
  const used = () => props.mem?.swapUsedBytes ?? 0;
  const pct = () => (total() > 0 ? (used() / total()) * 100 : 0);
  return (
    <div class="safai-card" style={{ padding: '18px' }}>
      <div
        style={{
          'font-size': '11px',
          color: 'var(--safai-fg-3)',
          'letter-spacing': '0.12em',
          'text-transform': 'uppercase',
          'margin-bottom': '10px',
        }}
      >
        Swap
      </div>
      <Show
        when={total() > 0}
        fallback={
          <div style={{ 'font-size': '12px', color: 'var(--safai-fg-3)' }}>
            Swap disabled or empty.
          </div>
        }
      >
        <div style={{ display: 'flex', 'align-items': 'baseline', gap: '8px', 'margin-bottom': '8px' }}>
          <span class="num" style={{ 'font-size': '18px', color: 'var(--safai-fg-0)', 'font-weight': 500 }}>
            {formatBytes(used())}
          </span>
          <span style={{ 'font-size': '12px', color: 'var(--safai-fg-3)' }}>
            of {formatBytes(total())}
          </span>
        </div>
        <div
          style={{
            height: '6px',
            'border-radius': '3px',
            background: 'var(--safai-bg-2)',
            overflow: 'hidden',
          }}
        >
          <div
            style={{
              height: '100%',
              width: `${Math.min(100, pct())}%`,
              background: 'var(--safai-amber, var(--safai-cyan))',
              transition: 'width 360ms ease-out',
            }}
          />
        </div>
      </Show>
    </div>
  );
}
