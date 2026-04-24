import { createSignal, onMount, Show, type JSX } from 'solid-js';
import { checkForUpdate, downloadWithProgress, type UpdateStatus } from '../lib/updater';
import { Icon } from './Icon';

// top-right update banner. checks once on mount, only shows if there's
// actually an update. click to download+relaunch, dismissable.
// isTauri() short-circuits on pnpm dev so we don't crash on missing plugin.
export function UpdateBanner(): JSX.Element {
  const [status, setStatus] = createSignal<UpdateStatus>({ kind: 'idle' });
  const [dismissed, setDismissed] = createSignal(false);

  onMount(async () => {
    setStatus({ kind: 'checking' });
    try {
      const update = await checkForUpdate();
      if (update == null) {
        setStatus({ kind: 'upToDate' });
        return;
      }
      setStatus({ kind: 'available', version: update.version, notes: update.notes });
    } catch (e) {
      // missing pubkey, offline, or signing mismatch ends up here.
      // we render nothing, don't block the user if the updater misfires.
      setStatus({ kind: 'error', message: String(e) });
    }
  });

  const applyUpdate = async () => {
    const s = status();
    if (s.kind !== 'available') return;
    setStatus({ kind: 'downloading', downloaded: 0, total: null });
    try {
      await downloadWithProgress((downloaded, total) => {
        setStatus({ kind: 'downloading', downloaded, total });
      });
      // relaunch() shouldn't return, process should be restarted. if it
      // does, flip to ready so user gets a manual restart affordance.
      setStatus({ kind: 'ready' });
    } catch (e) {
      setStatus({ kind: 'error', message: String(e) });
    }
  };

  const visible = () => {
    if (dismissed()) return false;
    const s = status();
    return (
      s.kind === 'available' ||
      s.kind === 'downloading' ||
      s.kind === 'ready' ||
      // only show errors if user was mid-install, silent check fail shouldn't nag
      (s.kind === 'error' && false)
    );
  };

  return (
    <Show when={visible()}>
      <div
        role="status"
        aria-live="polite"
        style={{
          position: 'fixed',
          top: '56px',
          right: '24px',
          'z-index': 1000,
          padding: '12px 16px',
          'max-width': '360px',
          'border-radius': 'var(--safai-r-md)',
          background: 'var(--safai-bg-2)',
          border: '1px solid var(--safai-cyan-dim)',
          'box-shadow': 'var(--safai-shadow)',
          display: 'flex',
          'flex-direction': 'column',
          gap: '8px',
        }}
      >
        <BannerBody status={status()} onApply={applyUpdate} onDismiss={() => setDismissed(true)} />
      </div>
    </Show>
  );
}

function BannerBody(props: {
  status: UpdateStatus;
  onApply: () => void;
  onDismiss: () => void;
}) {
  const s = props.status;
  if (s.kind === 'available') {
    return (
      <>
        <div style={{ display: 'flex', 'align-items': 'center', 'justify-content': 'space-between' }}>
          <div style={{ 'font-size': '13px', 'font-weight': 600 }}>
            Safai {s.version} is available
          </div>
          <button
            class="safai-btn safai-btn--ghost"
            onClick={props.onDismiss}
            aria-label="Dismiss update"
            style={{ height: '26px', padding: '0 6px' }}
          >
            <Icon name="x" size={10} />
          </button>
        </div>
        <Show when={s.notes}>
          <div style={{ 'font-size': '12px', color: 'var(--safai-fg-2)', 'white-space': 'pre-wrap' }}>
            {s.notes}
          </div>
        </Show>
        <button class="safai-btn safai-btn--primary" onClick={props.onApply}>
          Update and relaunch
        </button>
      </>
    );
  }
  if (s.kind === 'downloading') {
    const pct = s.total != null && s.total > 0 ? Math.round((s.downloaded / s.total) * 100) : null;
    return (
      <>
        <div style={{ 'font-size': '13px', 'font-weight': 600 }}>Downloading update…</div>
        <div class="safai-bar">
          <div
            class="safai-bar__fill"
            style={{ width: pct == null ? '35%' : `${pct}%`, transition: 'width 240ms ease-out' }}
          />
        </div>
        <div style={{ 'font-size': '11px', color: 'var(--safai-fg-3)' }}>
          {pct == null ? 'Starting…' : `${pct}%`}
        </div>
      </>
    );
  }
  if (s.kind === 'ready') {
    return (
      <div style={{ 'font-size': '13px' }}>Update installed - restart to apply.</div>
    );
  }
  return null;
}
