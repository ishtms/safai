import { createEffect, createSignal, onCleanup, type JSX } from 'solid-js';
import { countUpValue } from '../lib/animation';

interface CountUpProps {
  value: number;
  /** defaults to Intl tabular integer */
  format?: (v: number) => string;
  /** ease duration in ms, defaults 800 */
  duration?: number;
  /** start from 0 instead of last value */
  fromZero?: boolean;
  style?: JSX.CSSProperties;
  class?: string;
}

// animated number. uses rAF so backgrounded tabs pause and we don't
// fight solid's reconciler. cancels on unmount to avoid leaking the loop.
export function CountUp(props: CountUpProps): JSX.Element {
  const format = () => props.format ?? ((v: number) => Math.round(v).toLocaleString('en-US'));
  const duration = () => props.duration ?? 800;

  const [displayed, setDisplayed] = createSignal(props.fromZero ? 0 : props.value);

  let rafId: number | null = null;
  let startTime = 0;
  let fromValue = displayed();

  function cancelFrame() {
    if (rafId != null) {
      cancelAnimationFrame(rafId);
      rafId = null;
    }
  }

  createEffect(() => {
    const target = props.value;
    if (!Number.isFinite(target)) {
      setDisplayed(target);
      return;
    }
    cancelFrame();
    fromValue = displayed();
    startTime = performance.now();
    if (fromValue === target) return;

    const tick = (now: number) => {
      const elapsed = now - startTime;
      const next = countUpValue(fromValue, target, elapsed, duration());
      setDisplayed(next);
      if (elapsed >= duration()) {
        rafId = null;
        return;
      }
      rafId = requestAnimationFrame(tick);
    };
    rafId = requestAnimationFrame(tick);
  });

  onCleanup(cancelFrame);

  return (
    <span class={props.class} style={props.style}>
      {format()(displayed())}
    </span>
  );
}
