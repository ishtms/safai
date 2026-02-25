import { JSX } from 'solid-js';

export type IconName =
  | 'bolt' | 'pie' | 'broom' | 'copy' | 'archive' | 'shield' | 'shield2'
  | 'apps' | 'power' | 'chip' | 'pulse' | 'search' | 'settings' | 'trash'
  | 'check' | 'x' | 'plus' | 'minus' | 'chevronR' | 'chevronD' | 'download'
  | 'warning' | 'info' | 'sparkle' | 'file' | 'folder' | 'image' | 'clock'
  | 'star' | 'eye' | 'lock' | 'globe' | 'refresh' | 'pause' | 'play'
  | 'menu' | 'heart' | 'moon' | 'sun';

interface IconProps {
  name: IconName;
  size?: number;
  color?: string;
  strokeWidth?: number;
}

const PATHS: Record<string, JSX.Element> = {
  bolt: <path d="M9 1L3 9h4l-1 6 6-8H8l1-6z" />,
  pie: (<><circle cx="8" cy="8" r="6"/><path d="M8 2v6l4.2 4.2"/></>),
  broom: (<><path d="M10 2l4 4"/><path d="M3 13l6-6 4 4-6 6H3v-4z"/><path d="M3 13l-1 1"/></>),
  copy: (<><rect x="5" y="5" width="9" height="9" rx="1.5"/><path d="M2 10V3a1 1 0 0 1 1-1h7"/></>),
  archive: (<><rect x="2" y="3" width="12" height="3" rx="1"/><path d="M3 6v7a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1V6"/><path d="M6.5 9h3"/></>),
  shield: <path d="M8 1.5L2.5 4v4.5c0 3 2.3 5.5 5.5 6 3.2-.5 5.5-3 5.5-6V4L8 1.5z" />,
  shield2: (<><path d="M8 1.5L2.5 4v4.5c0 3 2.3 5.5 5.5 6 3.2-.5 5.5-3 5.5-6V4L8 1.5z"/><path d="M5.5 8l2 2L11 6.5"/></>),
  apps: (<><rect x="2" y="2" width="5" height="5" rx="1"/><rect x="9" y="2" width="5" height="5" rx="1"/><rect x="2" y="9" width="5" height="5" rx="1"/><rect x="9" y="9" width="5" height="5" rx="1"/></>),
  power: (<><path d="M8 2v5"/><path d="M4.2 5.2a5 5 0 1 0 7.6 0"/></>),
  chip: (<><rect x="4" y="4" width="8" height="8" rx="1.5"/><rect x="6.5" y="6.5" width="3" height="3"/><path d="M6 2v2M10 2v2M6 12v2M10 12v2M2 6h2M2 10h2M12 6h2M12 10h2"/></>),
  pulse: <path d="M1 8h3l2-5 4 10 2-5h3" />,
  search: (<><circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/></>),
  settings: (<><circle cx="8" cy="8" r="2"/><path d="M8 1v2M8 13v2M3.5 3.5l1.4 1.4M11.1 11.1l1.4 1.4M1 8h2M13 8h2M3.5 12.5l1.4-1.4M11.1 4.9l1.4-1.4"/></>),
  trash: (<><path d="M2.5 4h11M6 4V2.5a1 1 0 0 1 1-1h2a1 1 0 0 1 1 1V4"/><path d="M3.5 4l.8 9a1 1 0 0 0 1 .9h5.4a1 1 0 0 0 1-.9L12.5 4"/><path d="M6.5 7v4M9.5 7v4"/></>),
  check: <path d="M3 8l3.5 3.5L13 5" />,
  x: <path d="M3.5 3.5l9 9M12.5 3.5l-9 9" />,
  plus: <path d="M8 3v10M3 8h10" />,
  minus: <path d="M3 8h10" />,
  chevronR: <path d="M6 3l4 5-4 5" />,
  chevronD: <path d="M3 6l5 4 5-4" />,
  download: (<><path d="M8 2v8M4 7l4 4 4-4"/><path d="M2 13h12"/></>),
  warning: (<><path d="M8 2l6 11H2L8 2z"/><path d="M8 7v3"/><circle cx="8" cy="12" r="0.5" fill="currentColor"/></>),
  info: (<><circle cx="8" cy="8" r="6"/><path d="M8 7v4M8 5v.1"/></>),
  sparkle: <path d="M8 2l1.5 4L14 7.5 9.5 9 8 14 6.5 9 2 7.5 6.5 6z" />,
  file: (<><path d="M3 2h6l4 4v8a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V3a1 1 0 0 1 1-1z"/><path d="M9 2v4h4"/></>),
  folder: <path d="M2 4a1 1 0 0 1 1-1h3l2 2h5a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4z" />,
  image: (<><rect x="2" y="3" width="12" height="10" rx="1"/><circle cx="6" cy="7" r="1.3"/><path d="M2 11l3.5-3 4 4 2-2 2.5 2.5"/></>),
  clock: (<><circle cx="8" cy="8" r="6"/><path d="M8 4v4l2.5 2.5"/></>),
  star: <path d="M8 2l1.8 4.2 4.2.4-3.2 2.9 1 4.5L8 11.5 4.2 14l1-4.5L2 6.6l4.2-.4z" />,
  eye: (<><path d="M1 8s2.5-5 7-5 7 5 7 5-2.5 5-7 5-7-5-7-5z"/><circle cx="8" cy="8" r="2"/></>),
  lock: (<><rect x="3" y="7" width="10" height="7" rx="1"/><path d="M5 7V4.5a3 3 0 0 1 6 0V7"/></>),
  globe: (<><circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12"/></>),
  refresh: (<><path d="M13 3v4h-4"/><path d="M13 7a5 5 0 1 0 0 4"/></>),
  pause: (<><rect x="4" y="3" width="3" height="10"/><rect x="9" y="3" width="3" height="10"/></>),
  play: <path d="M4 2l9 6-9 6V2z" />,
  menu: <path d="M2 4h12M2 8h12M2 12h12" />,
  heart: <path d="M8 14S2 10 2 6a3 3 0 0 1 6-1 3 3 0 0 1 6 1c0 4-6 8-6 8z" />,
  moon: <path d="M13 9A6 6 0 1 1 7 3a5 5 0 0 0 6 6z" />,
  sun: (<><circle cx="8" cy="8" r="3"/><path d="M8 1v2M8 13v2M3 3l1.5 1.5M11.5 11.5L13 13M1 8h2M13 8h2M3 13l1.5-1.5M11.5 4.5L13 3"/></>),
};

const FILL_SHAPES = new Set(['bolt', 'sparkle', 'star', 'folder', 'play', 'heart', 'moon']);

export function Icon(props: IconProps) {
  const size = () => props.size ?? 16;
  const color = () => props.color ?? 'currentColor';
  const strokeWidth = () => props.strokeWidth ?? 1.6;
  const isFill = () => FILL_SHAPES.has(props.name);
  const path = () => PATHS[props.name] ?? <circle cx="8" cy="8" r="5" />;

  return (
    <svg
      width={size()}
      height={size()}
      viewBox="0 0 16 16"
      fill="none"
      stroke={color()}
      stroke-width={strokeWidth()}
      stroke-linecap="round"
      stroke-linejoin="round"
      style={isFill()
        ? { color: color(), fill: color(), stroke: 'none' }
        : { color: color() }}
    >
      {path()}
    </svg>
  );
}
