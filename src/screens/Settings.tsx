// settings. 4 tabs off a single bundle: one fetch on mount, one on save.
// everything applies atomically on Save. no auto-save per toggle - a mid-edit
// save would push a partial cadence change to the scheduler and fire a scan
// the user hadn't confirmed.

import {
  createEffect,
  createResource,
  createSignal,
  For,
  Match,
  Show,
  Switch,
  type JSX,
} from 'solid-js';
import { SafaiToolbar } from '../components/SafaiToolbar';
import { Suds } from '../components/Suds';
import { Icon } from '../components/Icon';
import { formatBytes, formatRelativeTime } from '../lib/format';
import { resetOnboarding, type Preferences, type ScheduleCadence } from '../lib/onboarding';
import {
  formatRelativeSecs,
  formatTimestamp,
  getSettings,
  labelForCadence,
  resetPrefs,
  updateSettings,
  type SettingsBundle,
} from '../lib/settings';
import { invalidateFilesystemCachesSoon } from '../lib/cacheInvalidation';

type Tab = 'general' | 'scanning' | 'privacy' | 'about';

const TABS: Array<{ id: Tab; label: string; icon: Parameters<typeof Icon>[0]['name'] }> = [
  { id: 'general', label: 'General', icon: 'settings' },
  { id: 'scanning', label: 'Scanning', icon: 'bolt' },
  { id: 'privacy', label: 'Privacy', icon: 'shield' },
  { id: 'about', label: 'About', icon: 'info' },
];

const CADENCES: Array<{ id: ScheduleCadence | null; label: string; hint: string }> = [
  { id: null, label: 'Off', hint: 'Only scan when I click' },
  { id: 'daily', label: 'Daily', hint: 'Once every 24 hours' },
  { id: 'weekly', label: 'Weekly', hint: 'Once every 7 days' },
  { id: 'monthly', label: 'Monthly', hint: 'Once every 30 days' },
];

export default function Settings(): JSX.Element {
  const [bundle, { refetch }] = createResource(getSettings);
  const [tab, setTab] = createSignal<Tab>('general');
  // draft mirrors server bundle, mutates locally. Save is the only
  // place draft -> server happens. keeps the scheduler (persisted state)
  // decoupled from the UI (draft)
  const [draft, setDraft] = createSignal<SettingsBundle | null>(null);
  const [status, setStatus] = createSignal<{ kind: 'ok' | 'err'; text: string } | null>(null);
  const [saving, setSaving] = createSignal(false);

  // seed draft from first server response. reactive so late-resolving
  // resources (slow disk, tauri) still hydrate
  const syncDraft = (b: SettingsBundle) => {
    setDraft({
      ...b,
      prefs: { ...b.prefs, includedCategories: [...b.prefs.includedCategories] },
    });
  };
  createEffect(() => {
    const b = bundle();
    if (b && !draft()) {
      syncDraft(b);
    }
  });

  const dirty = () => {
    const b = bundle();
    const d = draft();
    if (!b || !d) return false;
    return JSON.stringify(d.prefs) !== JSON.stringify(b.prefs) || d.telemetryOptIn !== b.telemetryOptIn;
  };

  const save = async () => {
    const d = draft();
    if (!d || saving()) return;
    setSaving(true);
    try {
      const updated = await updateSettings(d.prefs, d.telemetryOptIn);
      syncDraft(updated);
      invalidateFilesystemCachesSoon();
      setStatus({ kind: 'ok', text: 'Saved.' });
    } catch (e) {
      setStatus({ kind: 'err', text: String((e as Error)?.message ?? e) });
    } finally {
      setSaving(false);
      setTimeout(() => setStatus(null), 3000);
    }
  };

  const resetAll = async () => {
    try {
      const updated = await resetPrefs();
      syncDraft(updated);
      invalidateFilesystemCachesSoon();
      setStatus({ kind: 'ok', text: 'Preferences reset to defaults.' });
    } catch (e) {
      setStatus({ kind: 'err', text: String((e as Error)?.message ?? e) });
    } finally {
      setTimeout(() => setStatus(null), 3000);
    }
  };

  const patchPrefs = (patch: Partial<Preferences>) => {
    setDraft((d) => (d ? { ...d, prefs: { ...d.prefs, ...patch } } : d));
  };

  return (
    <div
      style={{
        flex: 1,
        display: 'flex',
        'flex-direction': 'column',
        'min-height': 0,
        overflow: 'hidden',
        background: 'var(--safai-bg-0)',
      }}
    >
      <SafaiToolbar title="Settings" subtitle="Tune how Safai behaves for you" />

      <div style={{ flex: 1, display: 'flex', 'min-height': 0, overflow: 'hidden' }}>
        {/* Tab rail */}
        <aside
          style={{
            width: '200px',
            'flex-shrink': 0,
            padding: '16px 8px',
            'border-right': '1px solid var(--safai-line)',
            background: 'var(--safai-bg-1)',
            display: 'flex',
            'flex-direction': 'column',
            gap: '4px',
          }}
          role="tablist"
          aria-label="Settings tabs"
        >
          <For each={TABS}>
            {(t) => (
              <button
                role="tab"
                aria-selected={tab() === t.id}
                onClick={() => setTab(t.id)}
                class="safai-btn safai-btn--ghost"
                style={{
                  height: '34px',
                  'justify-content': 'flex-start',
                  background: tab() === t.id ? 'oklch(0.82 0.14 200 / 0.10)' : 'transparent',
                  border:
                    tab() === t.id
                      ? '1px solid oklch(0.82 0.14 200 / 0.25)'
                      : '1px solid transparent',
                }}
              >
                <Icon
                  name={t.icon}
                  size={13}
                  color={tab() === t.id ? 'var(--safai-cyan)' : 'var(--safai-fg-2)'}
                />
                <span style={{ 'margin-left': '8px' }}>{t.label}</span>
              </button>
            )}
          </For>
        </aside>

        {/* Panels */}
        <section
          style={{
            flex: 1,
            overflow: 'auto',
            padding: '24px 32px 32px',
            display: 'flex',
            'flex-direction': 'column',
            gap: '20px',
          }}
          role="tabpanel"
        >
          <Show
            when={draft()}
            fallback={
              <div style={{ color: 'var(--safai-fg-2)', padding: '24px' }}>
                <Suds size={48} mood="sleepy" /> Loading settings…
              </div>
            }
          >
            {(d) => (
              <Switch>
                <Match when={tab() === 'general'}>
                  <GeneralTab draft={d()} patchPrefs={patchPrefs} />
                </Match>
                <Match when={tab() === 'scanning'}>
                  <ScanningTab
                    draft={d()}
                    bundle={bundle()}
                    patchPrefs={patchPrefs}
                    refresh={refetch}
                  />
                </Match>
                <Match when={tab() === 'privacy'}>
                  <PrivacyTab draft={d()} />
                </Match>
                <Match when={tab() === 'about'}>
                  <AboutTab draft={d()} />
                </Match>
              </Switch>
            )}
          </Show>

          {/* save/reset bar, pinned bottom */}
          <Show when={draft()}>
            <div
              style={{
                display: 'flex',
                'align-items': 'center',
                gap: '12px',
                'padding-top': '16px',
                'border-top': '1px solid var(--safai-line)',
              }}
            >
              <button
                class="safai-btn safai-btn--primary"
                disabled={!dirty() || saving()}
                onClick={save}
              >
                {saving() ? 'Saving…' : 'Save changes'}
              </button>
              <button class="safai-btn safai-btn--ghost" onClick={resetAll}>
                Reset to defaults
              </button>
              <Show when={status()}>
                <span
                  style={{
                    'font-size': '12px',
                    color:
                      status()!.kind === 'ok'
                        ? 'oklch(0.78 0.14 140)'
                        : 'oklch(0.70 0.17 25)',
                  }}
                >
                  {status()!.text}
                </span>
              </Show>
            </div>
          </Show>
        </section>
      </div>
    </div>
  );
}

// general tab

function GeneralTab(props: {
  draft: SettingsBundle;
  patchPrefs: (p: Partial<Preferences>) => void;
}) {
  return (
    <div style={{ display: 'flex', 'flex-direction': 'column', gap: '18px' }}>
      <SectionCard
        title="On launch"
        body={
          <CheckboxRow
            label="Run a scan automatically every time I open Safai"
            checked={props.draft.prefs.autoScanOnLaunch}
            onChange={() =>
              props.patchPrefs({ autoScanOnLaunch: !props.draft.prefs.autoScanOnLaunch })
            }
          />
        }
      />
      <SectionCard
        title="Cleanup"
        body={
          <CheckboxRow
            label="Ask me before cleaning anything (recommended)"
            checked={props.draft.prefs.confirmBeforeClean}
            onChange={() =>
              props.patchPrefs({ confirmBeforeClean: !props.draft.prefs.confirmBeforeClean })
            }
          />
        }
      />
    </div>
  );
}

// scanning tab

function ScanningTab(props: {
  draft: SettingsBundle;
  bundle: SettingsBundle | undefined;
  patchPrefs: (p: Partial<Preferences>) => void;
  refresh: () => void;
}) {
  const CATS: Array<{ id: Preferences['includedCategories'][number]; label: string }> = [
    { id: 'system-junk', label: 'System Junk' },
    { id: 'duplicates', label: 'Duplicates' },
    { id: 'large-old', label: 'Large & Old' },
    { id: 'privacy', label: 'Privacy' },
    { id: 'app-leftovers', label: 'App leftovers' },
    { id: 'trash', label: 'Trash' },
  ];
  const toggleCat = (id: Preferences['includedCategories'][number]) => {
    const has = props.draft.prefs.includedCategories.includes(id);
    props.patchPrefs({
      includedCategories: has
        ? props.draft.prefs.includedCategories.filter((c) => c !== id)
        : [...props.draft.prefs.includedCategories, id],
    });
  };

  return (
    <div style={{ display: 'flex', 'flex-direction': 'column', gap: '18px' }}>
      <SectionCard
        title="Schedule"
        subtitle={`Next scheduled scan: ${formatRelativeSecs(
          props.bundle?.scheduler.secondsUntilNext ?? null,
        )} · last ran ${formatRelativeTime(
          props.bundle?.scheduler.lastRunAt ?? null,
        )}`}
        body={
          <div style={{ display: 'grid', 'grid-template-columns': 'repeat(2, 1fr)', gap: '8px' }}>
            <For each={CADENCES}>
              {(c) => (
                <button
                  class="safai-card safai-card--hover"
                  onClick={() => props.patchPrefs({ scheduledScan: c.id })}
                  aria-pressed={props.draft.prefs.scheduledScan === c.id}
                  style={{
                    padding: '12px 14px',
                    'text-align': 'left',
                    background:
                      props.draft.prefs.scheduledScan === c.id
                        ? 'color-mix(in oklab, var(--safai-cyan) 10%, var(--safai-bg-2))'
                        : 'var(--safai-bg-2)',
                    border:
                      props.draft.prefs.scheduledScan === c.id
                        ? '1px solid color-mix(in oklab, var(--safai-cyan) 60%, transparent)'
                        : '1px solid var(--safai-line)',
                    cursor: 'pointer',
                  }}
                >
                  <div
                    style={{
                      'font-weight': 500,
                      color: 'var(--safai-fg-0)',
                      'margin-bottom': '4px',
                    }}
                  >
                    {c.label}
                  </div>
                  <div style={{ color: 'var(--safai-fg-2)', 'font-size': '12px' }}>{c.hint}</div>
                </button>
              )}
            </For>
          </div>
        }
      />

      <SectionCard
        title="Scan these categories"
        subtitle="Which buckets Smart Scan will sweep"
        body={
          <div style={{ display: 'grid', 'grid-template-columns': 'repeat(2, 1fr)', gap: '8px' }}>
            <For each={CATS}>
              {(c) => (
                <CheckboxRow
                  label={c.label}
                  checked={props.draft.prefs.includedCategories.includes(c.id)}
                  onChange={() => toggleCat(c.id)}
                />
              )}
            </For>
          </div>
        }
      />

      <SectionCard
        title="Large & Old thresholds"
        subtitle={`Currently: files ≥ ${formatBytes(
          props.draft.prefs.largeMinBytes,
        )} and untouched for ≥ ${props.draft.prefs.largeMinDaysIdle} days`}
        body={
          <div style={{ display: 'grid', 'grid-template-columns': '1fr 1fr', gap: '12px' }}>
            <ThresholdInput
              label="Minimum size (bytes)"
              value={props.draft.prefs.largeMinBytes}
              min={1024 * 1024}
              step={1024 * 1024}
              hint={formatBytes(props.draft.prefs.largeMinBytes)}
              onChange={(v) => props.patchPrefs({ largeMinBytes: v })}
            />
            <ThresholdInput
              label="Minimum days untouched"
              value={props.draft.prefs.largeMinDaysIdle}
              min={30}
              step={30}
              hint={`${props.draft.prefs.largeMinDaysIdle} days`}
              onChange={(v) => props.patchPrefs({ largeMinDaysIdle: v })}
            />
          </div>
        }
      />
    </div>
  );
}

// privacy tab

function PrivacyTab(_props: { draft: SettingsBundle }) {
  return (
    <div style={{ display: 'flex', 'flex-direction': 'column', gap: '18px' }}>
      <SectionCard
        title="Re-run onboarding"
        subtitle="Clears onboarding state; next launch walks you through welcome → permissions → prefs → ready again."
        body={
          <button
            class="safai-btn safai-btn--ghost"
            onClick={async () => {
              await resetOnboarding();
              // hard-navigate so the gate picks up the cleared state
              if (typeof window !== 'undefined') {
                window.location.href = '/onboarding/welcome';
              }
            }}
          >
            Start over
          </button>
        }
      />
    </div>
  );
}

// about tab

function AboutTab(props: { draft: SettingsBundle }) {
  const onboardedAt = () => formatTimestamp(props.draft.completedAt);
  return (
    <div style={{ display: 'flex', 'flex-direction': 'column', gap: '18px' }}>
      <SectionCard
        title="About Safai"
        body={
          <div
            style={{
              display: 'flex',
              'flex-direction': 'column',
              gap: '10px',
              color: 'var(--safai-fg-1)',
              'font-size': '13px',
              'line-height': 1.6,
            }}
          >
            <div style={{ display: 'flex', 'align-items': 'center', gap: '12px' }}>
              <Suds size={48} mood="happy" />
              <div>
                <div
                  style={{
                    'font-weight': 600,
                    'font-size': '18px',
                    'font-family': 'var(--safai-font-display)',
                  }}
                >
                  Safai v{props.draft.appVersion}
                </div>
                <div style={{ color: 'var(--safai-fg-2)', 'font-size': '12px' }}>
                  Open-source cross-platform system cleaner
                </div>
              </div>
            </div>
            <Row k="Onboarded" v={onboardedAt()} />
            <Row k="Current cadence" v={labelForCadence(props.draft.scheduler.cadence)} />
            <Row k="Last scheduled fire" v={formatTimestamp(props.draft.scheduler.lastRunAt)} />
            <Row k="Next scheduled fire" v={formatTimestamp(props.draft.scheduler.nextRunAt)} />
          </div>
        }
      />
    </div>
  );
}

function Row(props: { k: string; v: string }) {
  return (
    <div
      style={{
        display: 'flex',
        'justify-content': 'space-between',
        gap: '16px',
        'font-size': '12px',
        color: 'var(--safai-fg-2)',
        padding: '4px 0',
        'border-bottom': '1px dashed var(--safai-line)',
      }}
    >
      <span>{props.k}</span>
      <span class="num" style={{ color: 'var(--safai-fg-0)' }}>
        {props.v}
      </span>
    </div>
  );
}

// shared

function SectionCard(props: { title: string; subtitle?: string; body: JSX.Element }) {
  return (
    <section
      class="safai-card"
      style={{
        padding: '18px 20px',
        display: 'flex',
        'flex-direction': 'column',
        gap: '14px',
      }}
    >
      <header>
        <div style={{ 'font-weight': 600, color: 'var(--safai-fg-0)', 'font-size': '14px' }}>
          {props.title}
        </div>
        <Show when={props.subtitle}>
          <div
            style={{
              color: 'var(--safai-fg-2)',
              'font-size': '12px',
              'margin-top': '4px',
              'line-height': 1.5,
            }}
          >
            {props.subtitle}
          </div>
        </Show>
      </header>
      <div>{props.body}</div>
    </section>
  );
}

function CheckboxRow(props: { label: string; checked: boolean; onChange: () => void }) {
  return (
    <button
      class="safai-card safai-card--hover"
      onClick={props.onChange}
      aria-pressed={props.checked}
      style={{
        padding: '10px 14px',
        display: 'flex',
        'align-items': 'center',
        gap: '12px',
        'text-align': 'left',
        cursor: 'pointer',
        background: props.checked
          ? 'color-mix(in oklab, var(--safai-cyan) 8%, var(--safai-bg-2))'
          : 'var(--safai-bg-2)',
        border: props.checked
          ? '1px solid color-mix(in oklab, var(--safai-cyan) 60%, transparent)'
          : '1px solid var(--safai-line)',
      }}
    >
      <div
        class={props.checked ? 'safai-check safai-check--on' : 'safai-check'}
        style={{ 'flex-shrink': 0 }}
      >
        <Show when={props.checked}>
          <Icon name="check" size={9} color="oklch(0.18 0.02 240)" strokeWidth={2.2} />
        </Show>
      </div>
      <span style={{ 'font-size': '13px', color: 'var(--safai-fg-0)' }}>{props.label}</span>
    </button>
  );
}

function ThresholdInput(props: {
  label: string;
  value: number;
  min: number;
  step: number;
  hint: string;
  onChange: (n: number) => void;
}) {
  return (
    <label
      class="safai-card"
      style={{ padding: '10px 14px', display: 'flex', 'flex-direction': 'column', gap: '6px' }}
    >
      <span style={{ 'font-size': '11px', color: 'var(--safai-fg-3)' }}>{props.label}</span>
      <input
        type="number"
        min={props.min}
        step={props.step}
        value={props.value}
        onInput={(e) => {
          const n = Number((e.currentTarget as HTMLInputElement).value);
          if (Number.isFinite(n) && n >= 0) props.onChange(n);
        }}
        style={{
          background: 'transparent',
          border: 'none',
          outline: 'none',
          color: 'var(--safai-fg-0)',
          'font-family': 'var(--safai-font-display)',
          'font-size': '18px',
          'font-variant-numeric': 'tabular-nums',
          padding: 0,
          width: '100%',
        }}
      />
      <span style={{ 'font-size': '11px', color: 'var(--safai-fg-2)' }}>{props.hint}</span>
    </label>
  );
}
