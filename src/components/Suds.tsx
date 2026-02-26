import { JSX, Show } from 'solid-js';

export type SudsMood = 'happy' | 'wink' | 'sleepy' | 'zoom' | 'shocked';

interface SudsProps {
  size?: number;
  mood?: SudsMood;
  float?: boolean;
  style?: JSX.CSSProperties;
}

// safai mascot. bubble of circles, five moods. 1:1 from design's primitives.jsx
export function Suds(props: SudsProps) {
  const size = () => props.size ?? 56;
  const mood = () => props.mood ?? 'happy';
  const gradId = () => `sg-${size()}-${mood()}`;

  return (
    <svg
      class={props.float ? 'safai-mascot-float' : ''}
      width={size()}
      height={size()}
      viewBox="0 0 100 100"
      style={props.style}
    >
      <defs>
        <radialGradient id={gradId()} cx="35%" cy="30%" r="70%">
          <stop offset="0%" stop-color="oklch(0.98 0.04 200)" />
          <stop offset="55%" stop-color="oklch(0.82 0.14 200)" />
          <stop offset="100%" stop-color="oklch(0.58 0.15 220)" />
        </radialGradient>
      </defs>
      {/* body */}
      <circle cx="50" cy="52" r="42" fill={`url(#${gradId()})`} />
      {/* rim */}
      <circle cx="50" cy="52" r="42" fill="none" stroke="oklch(1 0 0 / 0.25)" stroke-width="1" />
      {/* highlight */}
      <ellipse cx="36" cy="34" rx="11" ry="7" fill="oklch(1 0 0 / 0.55)" />
      <circle cx="64" cy="30" r="3" fill="oklch(1 0 0 / 0.4)" />
      {/* eyes / mouth */}
      <Show when={mood() === 'happy'}>
        <circle cx="40" cy="55" r="3.2" fill="oklch(0.14 0.02 240)" />
        <circle cx="62" cy="55" r="3.2" fill="oklch(0.14 0.02 240)" />
        <path d="M 41 66 Q 50 72 59 66" stroke="oklch(0.14 0.02 240)" stroke-width="2.2" stroke-linecap="round" fill="none" />
      </Show>
      <Show when={mood() === 'wink'}>
        <path d="M 36 55 Q 40 51 44 55" stroke="oklch(0.14 0.02 240)" stroke-width="2.4" stroke-linecap="round" fill="none" />
        <circle cx="62" cy="55" r="3.2" fill="oklch(0.14 0.02 240)" />
        <path d="M 41 66 Q 50 72 59 66" stroke="oklch(0.14 0.02 240)" stroke-width="2.2" stroke-linecap="round" fill="none" />
      </Show>
      <Show when={mood() === 'sleepy'}>
        <path d="M 36 56 Q 40 52 44 56" stroke="oklch(0.14 0.02 240)" stroke-width="2.4" stroke-linecap="round" fill="none" />
        <path d="M 58 56 Q 62 52 66 56" stroke="oklch(0.14 0.02 240)" stroke-width="2.4" stroke-linecap="round" fill="none" />
        <path d="M 44 67 Q 50 68 56 67" stroke="oklch(0.14 0.02 240)" stroke-width="2" stroke-linecap="round" fill="none" />
      </Show>
      <Show when={mood() === 'zoom'}>
        <circle cx="40" cy="54" r="4" fill="oklch(0.14 0.02 240)" />
        <circle cx="62" cy="54" r="4" fill="oklch(0.14 0.02 240)" />
        <circle cx="41" cy="53" r="1.2" fill="oklch(1 0 0 / 0.9)" />
        <circle cx="63" cy="53" r="1.2" fill="oklch(1 0 0 / 0.9)" />
        <path d="M 40 68 Q 50 62 60 68" stroke="oklch(0.14 0.02 240)" stroke-width="2.2" stroke-linecap="round" fill="none" />
      </Show>
      <Show when={mood() === 'shocked'}>
        <circle cx="40" cy="55" r="3.2" fill="oklch(0.14 0.02 240)" />
        <circle cx="62" cy="55" r="3.2" fill="oklch(0.14 0.02 240)" />
        <ellipse cx="50" cy="68" rx="4" ry="5" fill="oklch(0.14 0.02 240)" />
      </Show>
    </svg>
  );
}
