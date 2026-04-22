import { createSignal, onCleanup, onMount, type JSX } from 'solid-js';
import { dashOffsetForProgress } from '../lib/animation';

interface RadialSweepProps {
  /** 0..1 progress, null for indeterminate rotating sweep */
  progress?: number | null;
  /** diameter px, defaults 260 */
  size?: number;
  /** seconds per full rotation when indeterminate */
  rotationSec?: number;
  children?: JSX.Element;
}

// svg sweep ring. determinate = stroke-dashoffset arc easing via css
// transition so 250ms progress updates don't look chunky. indeterminate
// = rAF-driven transform rotate while the walker is pre-counting.
export function RadialSweep(props: RadialSweepProps): JSX.Element {
  const size = () => props.size ?? 260;
  const rotationSec = () => props.rotationSec ?? 2.4;

  const RADIUS = 46;
  const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

  const progressRaw = () => props.progress;
  const isDeterminate = () =>
    progressRaw() != null && Number.isFinite(progressRaw());
  const progress = () => {
    const p = progressRaw();
    if (p == null || !Number.isFinite(p)) return 0;
    return Math.max(0, Math.min(1, p));
  };

  const [rotation, setRotation] = createSignal(0);
  let rafId: number | null = null;
  let startTime = 0;

  const runRotation = () => {
    const frame = (now: number) => {
      if (startTime === 0) startTime = now;
      const elapsed = now - startTime;
      const period = rotationSec() * 1000;
      setRotation(((elapsed % period) / period) * 360);
      rafId = requestAnimationFrame(frame);
    };
    rafId = requestAnimationFrame(frame);
  };

  onMount(() => {
    if (!isDeterminate()) runRotation();
  });
  onCleanup(() => {
    if (rafId != null) cancelAnimationFrame(rafId);
  });

  return (
    <div
      style={{
        position: 'relative',
        width: `${size()}px`,
        height: `${size()}px`,
        'flex-shrink': 0,
      }}
    >
      {/* halo */}
      <div
        aria-hidden
        style={{
          position: 'absolute',
          inset: 0,
          'border-radius': '50%',
          background:
            'radial-gradient(circle, oklch(0.82 0.14 200 / 0.18), transparent 70%)',
          'pointer-events': 'none',
        }}
      />

      {/* dashed rings, inset scales off size so spacing holds at any diameter */}
      <div
        aria-hidden
        style={{
          position: 'absolute',
          inset: `${size() * 0.14}px`,
          'border-radius': '50%',
          border: '1px dashed oklch(0.82 0.14 200 / 0.3)',
          'pointer-events': 'none',
        }}
      />
      <div
        aria-hidden
        style={{
          position: 'absolute',
          inset: `${size() * 0.26}px`,
          'border-radius': '50%',
          border: '1px dashed oklch(0.82 0.14 200 / 0.2)',
          'pointer-events': 'none',
        }}
      />

      {/* sweep arc */}
      <svg
        aria-hidden
        viewBox="0 0 100 100"
        style={{
          position: 'absolute',
          inset: 0,
          width: '100%',
          height: '100%',
          transform: isDeterminate() ? 'rotate(-90deg)' : `rotate(${rotation()}deg)`,
          'transform-origin': 'center',
          transition: isDeterminate() ? 'transform 0.3s ease-out' : undefined,
          'pointer-events': 'none',
        }}
      >
        <defs>
          <linearGradient id="safai-sweep-grad" x1="0" y1="0" x2="1" y2="1">
            <stop offset="0%" stop-color="oklch(0.82 0.14 200)" stop-opacity="0" />
            <stop offset="40%" stop-color="oklch(0.82 0.14 200)" stop-opacity="0.85" />
            <stop offset="100%" stop-color="oklch(0.78 0.12 300)" stop-opacity="1" />
          </linearGradient>
        </defs>
        <circle
          cx="50"
          cy="50"
          r={RADIUS}
          fill="none"
          stroke="oklch(0.82 0.14 200 / 0.08)"
          stroke-width="1.2"
        />
        <circle
          cx="50"
          cy="50"
          r={RADIUS}
          fill="none"
          stroke="url(#safai-sweep-grad)"
          stroke-width="1.4"
          stroke-linecap="round"
          stroke-dasharray={String(CIRCUMFERENCE)}
          stroke-dashoffset={String(
            dashOffsetForProgress(isDeterminate() ? progress() : 0.3, CIRCUMFERENCE),
          )}
          style={{ transition: isDeterminate() ? 'stroke-dashoffset 0.35s ease-out' : undefined }}
        />
      </svg>

      {/* centre content absolute so ring stays the layout anchor */}
      <div
        style={{
          position: 'absolute',
          inset: 0,
          display: 'flex',
          'align-items': 'center',
          'justify-content': 'center',
        }}
      >
        {props.children}
      </div>
    </div>
  );
}
