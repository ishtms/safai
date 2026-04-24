import { For, Show, createMemo } from 'solid-js';
import { Suds } from './Suds';
import { Icon } from './Icon';
import { formatBytes, formatCount, truncateMiddle } from '../lib/format';
import type { DeletePlan } from '../lib/cleaner';

// confirm step between previewDelete and commitDelete. shows bytes, count,
// and anything the cleaner skipped for safety. backdrop click or escape cancels.
export function ConfirmDeleteModal(props: {
  plan: DeletePlan;
  committing: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const previewRows = createMemo(() => {
    // cap preview rows, don't push confirm button off-screen on big plans
    return props.plan.items.slice(0, 6);
  });
  const moreCount = () => Math.max(0, props.plan.items.length - previewRows().length);

  const handleKey = (e: KeyboardEvent) => {
    if (props.committing) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      props.onCancel();
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      props.onConfirm();
    }
  };

  return (
    <div
      style={{
        position: 'fixed',
        inset: 0,
        'z-index': 1000,
        display: 'flex',
        'align-items': 'center',
        'justify-content': 'center',
        background: 'oklch(0 0 0 / 0.55)',
        'backdrop-filter': 'blur(4px)',
        'padding': '24px',
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget && !props.committing) props.onCancel();
      }}
      onKeyDown={handleKey}
      role="dialog"
      aria-modal="true"
      aria-labelledby="safai-confirm-title"
    >
      <div
        class="safai-card"
        style={{
          width: '100%',
          'max-width': '520px',
          padding: '24px 28px',
          background: 'var(--safai-bg-1)',
          'box-shadow': '0 24px 80px oklch(0 0 0 / 0.6)',
        }}
      >
        <div style={{ display: 'flex', gap: '18px', 'align-items': 'flex-start' }}>
          <Suds size={52} mood="shocked" />
          <div style={{ flex: 1, 'min-width': 0 }}>
            <div
              id="safai-confirm-title"
              style={{ 'font-size': '16px', 'font-weight': 600, 'margin-bottom': '4px' }}
            >
              Clean {formatBytes(props.plan.totalBytes)}?
            </div>
            <div style={{ 'font-size': '12px', color: 'var(--safai-fg-2)', 'line-height': 1.5 }}>
              Moving{' '}
              <span class="num" style={{ color: 'var(--safai-fg-0)' }}>
                {formatCount(props.plan.totalCount)}
              </span>{' '}
              {props.plan.totalCount === 1 ? 'item' : 'items'} to Safai's trash. Nothing
              is hard-deleted - click Restore on the toolbar to bring them back.
              <Show when={props.plan.protectedCount > 0}>
                {' '}
                <span style={{ color: 'var(--safai-amber)' }}>
                  {formatCount(props.plan.protectedCount)} item
                  {props.plan.protectedCount === 1 ? '' : 's'} skipped for safety.
                </span>
              </Show>
            </div>
          </div>
        </div>

        <div
          style={{
            'margin-top': '18px',
            'border-top': '1px solid var(--safai-line)',
            'padding-top': '14px',
          }}
        >
          <div
            style={{
              'font-size': '11px',
              color: 'var(--safai-fg-3)',
              'letter-spacing': '0.12em',
              'text-transform': 'uppercase',
              'margin-bottom': '10px',
            }}
          >
            Paths
          </div>
          <For each={previewRows()}>
            {(it) => (
              <div
                style={{
                  display: 'flex',
                  'align-items': 'center',
                  gap: '10px',
                  padding: '6px 0',
                  'font-size': '11px',
                  color: it.protected ? 'var(--safai-fg-3)' : 'var(--safai-fg-1)',
                  'min-width': 0,
                }}
                title={it.protected ? (it.protectedReason ?? 'skipped') : it.path}
              >
                <Icon
                  name={it.kind === 'directory' ? 'folder' : 'file'}
                  size={11}
                  color={it.protected ? 'var(--safai-fg-3)' : 'var(--safai-cyan)'}
                />
                <span
                  class="mono"
                  style={{
                    flex: 1,
                    'min-width': 0,
                    'white-space': 'nowrap',
                    overflow: 'hidden',
                    'text-overflow': 'ellipsis',
                    'text-decoration': it.protected ? 'line-through' : 'none',
                  }}
                >
                  {truncateMiddle(it.path, 70)}
                </span>
                <span class="num" style={{ 'font-size': '11px', 'flex-shrink': 0 }}>
                  {it.protected ? 'skipped' : formatBytes(it.bytes)}
                </span>
              </div>
            )}
          </For>
          <Show when={moreCount() > 0}>
            <div
              style={{
                'font-size': '11px',
                color: 'var(--safai-fg-3)',
                padding: '4px 0',
              }}
            >
              … and {formatCount(moreCount())} more
            </div>
          </Show>
        </div>

        <div
          style={{
            'margin-top': '20px',
            display: 'flex',
            'justify-content': 'flex-end',
            gap: '10px',
          }}
        >
          <button
            class="safai-btn safai-btn--ghost"
            onClick={props.onCancel}
            disabled={props.committing}
          >
            Cancel
          </button>
          <button
            class="safai-btn safai-btn--primary"
            onClick={props.onConfirm}
            disabled={props.committing || props.plan.totalCount === 0}
            aria-busy={props.committing}
            autofocus
          >
            <span class={props.committing ? 'safai-spin' : ''} style={{ display: 'inline-flex' }}>
              <Icon
                name={props.committing ? 'refresh' : 'trash'}
                size={12}
                color="oklch(0.18 0.02 240)"
              />
            </span>{' '}
            {props.committing
              ? 'Cleaning…'
              : `Clean ${formatBytes(props.plan.totalBytes)}`}
          </button>
        </div>
      </div>
    </div>
  );
}
