import { JSX } from 'solid-js';

export type OS = 'mac' | 'win' | 'linux';

interface OSChipProps {
  os: OS;
}

const GLYPHS: Record<OS, { label: string; glyph: JSX.Element }> = {
  mac: {
    label: 'macOS',
    glyph: (
      <path d="M7.5 1.6c.3.8-.1 1.6-.6 2.2-.5.6-1.3 1.1-2.1 1-.1-.7.2-1.5.7-2 .5-.6 1.3-1.1 2-1.2zM9.6 10.5c-.3.7-.5 1-.9 1.6-.6.8-1.4 1.8-2.4 1.8-.9 0-1.1-.6-2.3-.6-1.2 0-1.5.6-2.4.6-1 0-1.8-.9-2.4-1.7C-.6 10.2-.8 7.5.7 6c.7-.7 1.7-1.2 2.7-1.2.9 0 1.5.5 2.3.5.7 0 1.2-.5 2.3-.5.9 0 1.9.5 2.6 1.3-2.3 1.3-1.9 4.4-.5 4.7z" />
    ),
  },
  win: {
    label: 'Windows',
    glyph: (
      <>
        <rect x="1" y="1" width="5.5" height="5.5" />
        <rect x="7.5" y="1" width="5.5" height="5.5" />
        <rect x="1" y="7.5" width="5.5" height="5.5" />
        <rect x="7.5" y="7.5" width="5.5" height="5.5" />
      </>
    ),
  },
  linux: {
    label: 'Linux',
    glyph: (
      <path d="M7 1c-1.5 0-2.8 1.2-2.8 2.7 0 .5.1 1 .4 1.5-1 .8-1.8 2-1.8 3.3 0 1.5 1 2.7 2.5 3.5.6.3 1.2.5 1.7.5.5 0 1.1-.2 1.7-.5 1.5-.8 2.5-2 2.5-3.5 0-1.3-.8-2.5-1.8-3.3.3-.5.4-1 .4-1.5C9.8 2.2 8.5 1 7 1zm-1 3.5c.4 0 .7.3.7.8s-.3.7-.7.7-.7-.3-.7-.7.3-.8.7-.8zm2 0c.4 0 .7.3.7.8s-.3.7-.7.7-.7-.3-.7-.7.3-.8.7-.8z" />
    ),
  },
};

export function OSChip(props: OSChipProps) {
  const cur = () => GLYPHS[props.os];
  return (
    <div class="safai-pill" style={{ background: 'oklch(0.22 0.010 240)' }}>
      <svg
        width="12"
        height="12"
        viewBox="0 0 14 14"
        fill="currentColor"
        style={{ color: 'var(--safai-fg-1)' }}
      >
        {cur().glyph}
      </svg>
      {cur().label}
    </div>
  );
}
