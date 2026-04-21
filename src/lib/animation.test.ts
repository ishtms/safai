import { describe, expect, it } from 'vitest';
import {
  countUpValue,
  dashOffsetForProgress,
  easeOutCubic,
  lerp,
  sweepAngle,
} from './animation';

describe('easeOutCubic', () => {
  it('returns 0 at t=0', () => {
    expect(easeOutCubic(0)).toBe(0);
  });
  it('returns 1 at t=1', () => {
    expect(easeOutCubic(1)).toBe(1);
  });
  it('clamps below zero', () => {
    expect(easeOutCubic(-5)).toBe(0);
  });
  it('clamps above one', () => {
    expect(easeOutCubic(12)).toBe(1);
  });
  it('is monotonically increasing across the window', () => {
    let prev = -Infinity;
    for (let i = 0; i <= 10; i++) {
      const v = easeOutCubic(i / 10);
      expect(v).toBeGreaterThanOrEqual(prev);
      prev = v;
    }
  });
  it('decelerates: value at 0.5 > 0.5', () => {
    // fast-then-slow, half elapsed should be well past halfway visually
    expect(easeOutCubic(0.5)).toBeGreaterThan(0.5);
  });
});

describe('lerp', () => {
  it('returns from at t=0', () => {
    expect(lerp(10, 20, 0)).toBe(10);
  });
  it('returns to at t=1', () => {
    expect(lerp(10, 20, 1)).toBe(20);
  });
  it('clamps t below zero', () => {
    expect(lerp(10, 20, -1)).toBe(10);
  });
  it('clamps t above one', () => {
    expect(lerp(10, 20, 2)).toBe(20);
  });
  it('interpolates midpoint', () => {
    expect(lerp(0, 100, 0.5)).toBe(50);
  });
  it('handles from > to (countdown)', () => {
    expect(lerp(100, 0, 0.25)).toBe(75);
  });
});

describe('countUpValue', () => {
  it('returns target when durationMs is 0', () => {
    expect(countUpValue(0, 100, 50, 0)).toBe(100);
  });
  it('returns from at elapsed=0', () => {
    expect(countUpValue(10, 100, 0, 1000)).toBe(10);
  });
  it('returns target when elapsed >= duration', () => {
    expect(countUpValue(10, 100, 2000, 1000)).toBe(100);
    expect(countUpValue(10, 100, 1000, 1000)).toBe(100);
  });
  it('clamps negative elapsed to from', () => {
    expect(countUpValue(10, 100, -50, 1000)).toBe(10);
  });
  it('eases monotonically between endpoints', () => {
    let prev = -Infinity;
    for (let t = 0; t <= 1000; t += 100) {
      const v = countUpValue(0, 100, t, 1000);
      expect(v).toBeGreaterThanOrEqual(prev);
      prev = v;
    }
  });
  it('reaches target exactly at elapsed=duration (no float drift)', () => {
    expect(countUpValue(0, 1234567, 1000, 1000)).toBe(1234567);
  });
  it('passes through non-finite target untouched', () => {
    expect(countUpValue(0, NaN, 500, 1000)).toBeNaN();
  });
  it('handles negative-direction animation (countdown)', () => {
    const v = countUpValue(100, 0, 500, 1000);
    expect(v).toBeLessThan(100);
    expect(v).toBeGreaterThan(0);
  });
});

describe('dashOffsetForProgress', () => {
  const C = 2 * Math.PI * 46;
  it('full stroke hidden at progress=0', () => {
    expect(dashOffsetForProgress(0, C)).toBeCloseTo(C, 6);
  });
  it('stroke fully drawn at progress=1', () => {
    expect(dashOffsetForProgress(1, C)).toBe(0);
  });
  it('half-drawn at progress=0.5', () => {
    expect(dashOffsetForProgress(0.5, C)).toBeCloseTo(C / 2, 6);
  });
  it('clamps progress above 1', () => {
    expect(dashOffsetForProgress(2, C)).toBe(0);
  });
  it('clamps progress below 0', () => {
    expect(dashOffsetForProgress(-0.5, C)).toBeCloseTo(C, 6);
  });
});

describe('sweepAngle', () => {
  it('returns 0 at the start of the cycle', () => {
    expect(sweepAngle(0, 2000)).toBe(0);
  });
  it('returns 180 at half-period', () => {
    expect(sweepAngle(1000, 2000)).toBe(180);
  });
  it('wraps around at full period', () => {
    expect(sweepAngle(2000, 2000)).toBe(0);
  });
  it('handles large wall-clock values via modulo', () => {
    // hour-old ts should still land in [0, 360)
    const hourMs = 3_600_000;
    const a = sweepAngle(hourMs, 2000);
    expect(a).toBeGreaterThanOrEqual(0);
    expect(a).toBeLessThan(360);
  });
  it('handles negative wall-clock via modulo', () => {
    const a = sweepAngle(-500, 2000);
    expect(a).toBeGreaterThanOrEqual(0);
    expect(a).toBeLessThan(360);
  });
  it('returns 0 when periodMs is non-positive', () => {
    expect(sweepAngle(500, 0)).toBe(0);
    expect(sweepAngle(500, -1000)).toBe(0);
  });
});
