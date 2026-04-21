// pure animation helpers, no dom refs so they're unit-testable.
// count-up, ease-out, radial sweep math. solid components consume these.

// ease-out cubic, smooth deceleration that looks right for a number
// climbing to a target. not linear, not bouncy
export function easeOutCubic(t: number): number {
  const clamped = Math.max(0, Math.min(1, t));
  const inv = 1 - clamped;
  return 1 - inv * inv * inv;
}

// t clamped to [0,1] so callers don't deal with overshoot when a frame
// arrives late
export function lerp(from: number, to: number, t: number): number {
  const clamped = Math.max(0, Math.min(1, t));
  return from + (to - from) * clamped;
}

// count-up math. returns target exactly when elapsed >= duration or
// duration <= 0 so finished anims are bit-exact regardless of curve
// precision. no solid imports so it's trivially testable
export function countUpValue(
  from: number,
  to: number,
  elapsedMs: number,
  durationMs: number,
): number {
  if (!Number.isFinite(to)) return to;
  if (!Number.isFinite(from)) return to;
  if (durationMs <= 0) return to;
  if (elapsedMs >= durationMs) return to;
  if (elapsedMs <= 0) return from;
  return lerp(from, to, easeOutCubic(elapsedMs / durationMs));
}

// 0..1 -> stroke-dashoffset for a ring of the given circumference.
// lets the svg sweep fill without allocating a fresh path per frame
export function dashOffsetForProgress(
  progress: number,
  circumference: number,
): number {
  const clamped = Math.max(0, Math.min(1, progress));
  return circumference * (1 - clamped);
}

// degrees for an indeterminate rotating sweep. pure so we can drive it
// from one raf loop without components owning time
export function sweepAngle(nowMs: number, periodMs: number): number {
  if (periodMs <= 0) return 0;
  const cycle = ((nowMs % periodMs) + periodMs) % periodMs;
  return (cycle / periodMs) * 360;
}
