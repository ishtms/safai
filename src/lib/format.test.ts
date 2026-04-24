import { describe, expect, it } from 'vitest';
import { truncateMiddle } from './format';

describe('truncateMiddle', () => {
  it('returns short paths unchanged', () => {
    expect(truncateMiddle('/etc/hosts', 90)).toBe('/etc/hosts');
  });

  it('returns the input when at exactly max length', () => {
    const p = '/a'.repeat(45);
    expect(p.length).toBe(90);
    expect(truncateMiddle(p, 90)).toBe(p);
  });

  it('collapses the middle with an ellipsis when too long', () => {
    const p =
      '/Users/ish/Library/Caches/com.example.VeryLongBundleIdentifier/Sub/Folder/file-with-a-long-name.ext';
    const out = truncateMiddle(p, 60);
    expect(out.length).toBeLessThanOrEqual(60);
    expect(out).toContain('…');
    // filename tail must be preserved
    expect(out.endsWith('file-with-a-long-name.ext')).toBe(true);
    // head prefix visible
    expect(out.startsWith('/Users/')).toBe(true);
  });

  it('handles empty strings', () => {
    expect(truncateMiddle('', 90)).toBe('');
  });

  it('respects a small max', () => {
    const out = truncateMiddle('/a/very/long/path/to/a/file.txt', 20);
    expect(out.length).toBeLessThanOrEqual(20);
    expect(out).toContain('…');
  });
});
