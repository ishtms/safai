import { createMemo, createSignal, For, type JSX } from 'solid-js';

interface VisibleItem<T> {
  item: T;
  index: number;
}

export interface VirtualListProps<T> {
  items: readonly T[];
  itemHeight: number;
  maxHeight: number;
  ariaLabel?: string;
  threshold?: number;
  overscan?: number;
  children: (item: T, index: number) => JSX.Element;
}

export function VirtualList<T>(props: VirtualListProps<T>): JSX.Element {
  const [scrollTop, setScrollTop] = createSignal(0);
  const threshold = () => props.threshold ?? 200;
  const overscan = () => props.overscan ?? 6;
  const rowHeight = () => Math.max(1, props.itemHeight);
  const enabled = () => props.items.length > threshold();
  const maxHeight = () => Math.max(rowHeight(), props.maxHeight);
  const viewportHeight = () =>
    enabled()
      ? maxHeight()
      : Math.min(maxHeight(), Math.max(rowHeight(), props.items.length * rowHeight()));

  const range = createMemo(() => {
    const len = props.items.length;
    if (!enabled()) {
      return { start: 0, end: len, before: 0, after: 0 };
    }
    const h = rowHeight();
    const visible = Math.min(len, Math.ceil(viewportHeight() / h) + overscan() * 2);
    const requestedStart = Math.max(0, Math.floor(scrollTop() / h) - overscan());
    const start = Math.min(requestedStart, Math.max(0, len - visible));
    const end = Math.min(len, start + visible);
    return {
      start,
      end,
      before: start * h,
      after: Math.max(0, (len - end) * h),
    };
  });

  const visible = createMemo<VisibleItem<T>[]>(() => {
    const r = range();
    return props.items.slice(r.start, r.end).map((item, i) => ({
      item,
      index: r.start + i,
    }));
  });

  return (
    <div
      aria-label={props.ariaLabel}
      data-virtualized={enabled() ? 'true' : 'false'}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
      style={{
        height: `${viewportHeight()}px`,
        'overflow-y': enabled() ? 'auto' : 'visible',
        'overflow-x': 'hidden',
        'contain': enabled() ? 'strict' : undefined,
      }}
    >
      <div style={{ height: `${range().before}px` }} />
      <For each={visible()}>
        {(entry) => props.children(entry.item, entry.index)}
      </For>
      <div style={{ height: `${range().after}px` }} />
    </div>
  );
}
