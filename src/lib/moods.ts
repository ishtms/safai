import { createSignal, onCleanup, type Accessor } from 'solid-js';
import type { SudsMood } from '../components/Suds';

// post-action mood flash. accessor reads `transient` for `ms` after
// flash() then falls back to `base`. used on junk/dupes/privacy/malware
// so suds winks after a successful clean.
// single timer, repeated flashes reset it so back-to-back cleans don't
// stack or drop the wink early
export function useFlashMood(args: {
  base: Accessor<SudsMood>;
  transient: SudsMood;
  ms?: number;
}): { mood: Accessor<SudsMood>; flash: () => void } {
  const durationMs = args.ms ?? 2400;
  const [flashing, setFlashing] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  const flash = () => {
    if (timer != null) clearTimeout(timer);
    setFlashing(true);
    timer = setTimeout(() => {
      setFlashing(false);
      timer = null;
    }, durationMs);
  };

  onCleanup(() => {
    if (timer != null) clearTimeout(timer);
  });

  return {
    mood: () => (flashing() ? args.transient : args.base()),
    flash,
  };
}
