import { defineConfig } from 'vitest/config';

// — pure-math test config. No jsdom/happy-dom needed because the
// tested helpers in `src/lib/animation.ts` are environment-free; forcing
// `node` skips the jsdom dependency and cuts test start-up to ~50 ms.
export default defineConfig({
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
