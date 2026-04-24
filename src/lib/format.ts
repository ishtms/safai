// number formatting helpers used across the ui

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'] as const;

export function formatBytes(bytes: number, digits = 1): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
  const i = Math.min(UNITS.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
  const v = bytes / Math.pow(1024, i);
  return `${v.toFixed(i === 0 ? 0 : digits)} ${UNITS[i]}`;
}

export function formatCount(n: number): string {
  return n.toLocaleString('en-US');
}

export function formatDuration(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(s / 60);
  return `${m}:${String(s % 60).padStart(2, '0')}`;
}

// "2 min ago" style. unixSeconds null means never scanned.
// now is injectable for tests
export function formatRelativeTime(
  unixSeconds: number | null,
  now: number = Date.now(),
): string {
  if (unixSeconds == null) return 'Never';
  const diffSec = Math.max(0, Math.floor(now / 1000 - unixSeconds));
  if (diffSec < 10) return 'Just now';
  if (diffSec < 60) return `${diffSec}s ago`;
  const diffMin = Math.floor(diffSec / 60);
  if (diffMin < 60) return `${diffMin} min ago`;
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) return `${diffHr}h ago`;
  const diffDay = Math.floor(diffHr / 24);
  return `${diffDay}d ago`;
}

// ellipsis-in-the-middle so both ends stay readable. "~/Library/Caches/
// com.apple.FooBarBaz/..." truncated to 80 keeps $HOME prefix + filename
// visible, which is what users actually scan the path for. CSS ellipsis
// alone hides the filename (end of the string) which is worse.
//
// max defaults to 90. anything shorter than max returns as-is. preserves
// windows + unix separators.
export function truncateMiddle(path: string, max = 90): string {
  if (!path) return path;
  if (path.length <= max) return path;
  if (max <= 3) return '…';
  // aim for ~60% on the tail (filename-heavy), rest on the head.
  // budget leaves one char for the ellipsis glyph
  const budget = max - 1;
  const tail = Math.max(1, Math.floor(budget * 0.6));
  const head = Math.max(1, budget - tail);
  return `${path.slice(0, head)}…${path.slice(path.length - tail)}`;
}

// split bytes into value + unit so hero card can style them separately.
// same rules as formatBytes (base-1024, capped at PB)
export function splitBytes(bytes: number, digits = 1): { value: string; unit: string } {
  if (!Number.isFinite(bytes) || bytes <= 0) return { value: '0', unit: 'B' };
  const i = Math.min(UNITS.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
  const v = bytes / Math.pow(1024, i);
  return { value: v.toFixed(i === 0 ? 0 : digits), unit: UNITS[i] };
}
