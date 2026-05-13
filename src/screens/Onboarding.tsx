import {
  createResource,
  createSignal,
  For,
  Show,
  onMount,
  type JSX,
} from 'solid-js';
import { useNavigate } from '@solidjs/router';
import { Suds, type SudsMood } from '../components/Suds';
import { Icon } from '../components/Icon';
import { OSChip, type OS } from '../components/OSChip';
import { detectOS } from '../lib/platform';
import { formatBytes } from '../lib/format';
import {
  completeOnboarding,
  descriptionForKind,
  defaultPreferences,
  fetchPermissionStatus,
  getOnboardingState,
  labelForKind,
  ONBOARDING_STEPS,
  openPermissionSettings,
  recordPermission,
  saveOnboardingPrefs,
  setOnboardingStep,
  stepIndex,
  type IncludedCategory,
  type OnboardingState,
  type OnboardingStep,
  type PermissionKind,
  type PermissionStatus,
  type PermissionStatusEntry,
  type Preferences,
} from '../lib/onboarding';

// onboarding. 4 steps as distinct routes so browser back/forward maps
// to step boundaries and a crash resumes from persisted lastStep.
// shell holds suds + stepper + nav buttons so each step just renders
// its content. every step persists through a rust command before
// advancing, no "i picked a pref but it didn't save" limbo.

interface StepShellProps {
  step: OnboardingStep;
  title: string;
  subtitle: string;
  mood: SudsMood;
  children: JSX.Element;
  onBack?: (() => void) | null;
  primaryLabel: string;
  primaryDisabled?: boolean;
  onPrimary: () => void;
  secondary?: { label: string; onClick: () => void };
}

function StepShell(props: StepShellProps) {
  return (
    <div
      class="safai-root"
      style={{
        flex: 1,
        display: 'flex',
        'flex-direction': 'column',
        'min-height': '100vh',
        background:
          'linear-gradient(180deg, oklch(0.14 0.01 245) 0%, oklch(0.18 0.02 255) 100%)',
      }}
    >
      <Stepper current={props.step} />
      <div
        style={{
          flex: 1,
          display: 'flex',
          'align-items': 'center',
          'justify-content': 'center',
          padding: '32px 48px',
        }}
      >
        <div
          class="safai-card safai-sheen"
          style={{
            width: 'min(620px, 100%)',
            padding: '40px 44px',
            background:
              'linear-gradient(135deg, oklch(0.22 0.02 245), oklch(0.20 0.02 260))',
            border: '1px solid oklch(0.82 0.14 200 / 0.25)',
          }}
        >
          <div style={{ display: 'flex', 'align-items': 'center', gap: '20px', 'margin-bottom': '24px' }}>
            <Suds size={80} mood={props.mood} float />
            <div style={{ flex: 1 }}>
              <div
                style={{
                  'font-size': '22px',
                  'font-weight': 600,
                  color: 'var(--safai-fg-0)',
                  'font-family': 'var(--safai-font-display)',
                  'letter-spacing': '-0.02em',
                  'margin-bottom': '4px',
                }}
              >
                {props.title}
              </div>
              <div style={{ 'font-size': '13px', color: 'var(--safai-fg-2)', 'line-height': 1.5 }}>
                {props.subtitle}
              </div>
            </div>
          </div>
          <div style={{ 'margin-bottom': '28px' }}>{props.children}</div>
          <div
            style={{
              display: 'flex',
              'align-items': 'center',
              'justify-content': 'space-between',
              gap: '12px',
            }}
          >
            <div>
              <Show when={props.onBack}>
                <button
                  class="safai-btn safai-btn--ghost"
                  onClick={() => props.onBack?.()}
                >
                  <span style={{ 'font-size': '12px' }}>&larr;</span> Back
                </button>
              </Show>
            </div>
            <div style={{ display: 'flex', gap: '8px' }}>
              <Show when={props.secondary}>
                <button
                  class="safai-btn safai-btn--ghost"
                  onClick={() => props.secondary!.onClick()}
                >
                  {props.secondary!.label}
                </button>
              </Show>
              <button
                class="safai-btn safai-btn--primary"
                disabled={props.primaryDisabled}
                onClick={props.onPrimary}
              >
                {props.primaryLabel}{' '}
                <Icon name="chevronR" size={12} color="oklch(0.18 0.02 240)" />
              </button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}

function Stepper(props: { current: OnboardingStep }) {
  return (
    <div
      style={{
        display: 'flex',
        'align-items': 'center',
        'justify-content': 'center',
        gap: '10px',
        padding: '32px 0 0',
      }}
      role="progressbar"
      aria-valuemin={1}
      aria-valuemax={ONBOARDING_STEPS.length}
      aria-valuenow={Math.min(stepIndex(props.current) + 1, ONBOARDING_STEPS.length)}
    >
      <For each={ONBOARDING_STEPS}>
        {(s, i) => {
          const activeIdx = stepIndex(props.current);
          const isActive = i() === activeIdx;
          const isPast = i() < activeIdx;
          return (
            <div style={{ display: 'flex', 'align-items': 'center', gap: '10px' }}>
              <div
                style={{
                  width: isActive ? '24px' : '8px',
                  height: '8px',
                  'border-radius': '4px',
                  background: isActive
                    ? 'var(--safai-cyan)'
                    : isPast
                      ? 'color-mix(in oklab, var(--safai-cyan) 60%, transparent)'
                      : 'var(--safai-bg-3)',
                  transition: 'width 160ms ease-out, background-color 160ms',
                }}
                title={s}
              />
            </div>
          );
        }}
      </For>
    </div>
  );
}

// step 1: welcome

function WelcomeStep(props: { onNext: () => void }) {
  const [os, setOs] = createSignal<OS>('mac');
  onMount(async () => setOs(await detectOS()));

  return (
    <StepShell
      step="welcome"
      title="Hi, I'm Suds"
      subtitle="I clean up the fluff on your computer so it feels fast again. No ads, no spyware - it's just us."
      mood="happy"
      onBack={null}
      primaryLabel="Let's go"
      onPrimary={props.onNext}
    >
      <div
        style={{
          display: 'grid',
          'grid-template-columns': 'auto 1fr',
          gap: '14px 16px',
          'font-size': '13px',
          color: 'var(--safai-fg-1)',
          'line-height': 1.5,
        }}
      >
        <Bullet
          icon="broom"
          title="I find the junk"
          body="Caches, logs, old package manager bits - things your apps will happily regenerate."
        />
        <Bullet
          icon="shield"
          title="Nothing leaves this machine"
          body="Every scan runs locally. I never upload your file names, hashes, or contents anywhere."
        />
        <Bullet
          icon="sparkle"
          title="You stay in control"
          body="Nothing gets deleted without your explicit click, and everything I move can be restored."
        />
      </div>
      <div
        style={{
          'margin-top': '22px',
          display: 'flex',
          'align-items': 'center',
          gap: '10px',
          'font-size': '11px',
          color: 'var(--safai-fg-3)',
        }}
      >
        <OSChip os={os()} />
        <span>I'll tailor the flow to your OS.</span>
      </div>
    </StepShell>
  );
}

function Bullet(props: { icon: string; title: string; body: string }) {
  return (
    <>
      <div
        style={{
          width: '32px',
          height: '32px',
          'border-radius': '8px',
          background: 'color-mix(in oklab, var(--safai-cyan) 15%, transparent)',
          display: 'flex',
          'align-items': 'center',
          'justify-content': 'center',
          'align-self': 'start',
        }}
      >
        <Icon name={props.icon as 'broom'} size={14} color="var(--safai-cyan)" />
      </div>
      <div>
        <div style={{ 'font-weight': 500, color: 'var(--safai-fg-0)', 'margin-bottom': '2px' }}>
          {props.title}
        </div>
        <div style={{ color: 'var(--safai-fg-2)', 'font-size': '12px' }}>{props.body}</div>
      </div>
    </>
  );
}

// step 2: permissions

function PermissionsStep(props: { state: OnboardingState; onNext: () => void; onBack: () => void; onMutate: (s: OnboardingState) => void }) {
  const [perms, { refetch }] = createResource(fetchPermissionStatus);
  const [busy, setBusy] = createSignal<PermissionKind | null>(null);

  const handleOpen = async (kind: PermissionKind) => {
    setBusy(kind);
    try {
      await openPermissionSettings(kind);
    } catch {
      // settings launcher failures show up in the ui below
    } finally {
      setBusy(null);
    }
  };

  const handleVerdict = async (kind: PermissionKind, status: PermissionStatus) => {
    const s = await recordPermission(kind, status);
    props.onMutate(s);
    // refresh live probe so the dot updates without a reload
    refetch();
  };

  // prefer live probe when it says granted, so a user who granted FDA
  // in system settings passes the gate without clicking the in-app
  // Granted button. otherwise use persisted verdict, then probe.
  const effectiveStatus = (kind: PermissionKind, probed: PermissionStatus): PermissionStatus => {
    if (probed === 'granted') return 'granted';
    const persisted = props.state.permissions.find((r) => r.kind === kind)?.status;
    return persisted ?? probed;
  };

  // every applicable permission must be granted, empty list trivially
  // passes. can't claim "clean your home dir" without being able to read
  // it, and a stale denied from last session shouldn't silently break
  // scans
  const canContinue = () => {
    const list = perms() ?? [];
    if (list.length === 0) return true;
    return list.every((p) => effectiveStatus(p.kind, p.status) === 'granted');
  };

  const pendingCount = () => {
    const list = perms() ?? [];
    return list.filter((p) => effectiveStatus(p.kind, p.status) !== 'granted').length;
  };

  return (
    <StepShell
      step="permissions"
      title="A few permissions"
      subtitle="I need access to read your files before I can look for anything to clean up. Nothing leaves this machine and nothing gets deleted without your explicit confirmation."
      mood="wink"
      onBack={props.onBack}
      primaryLabel={canContinue() ? 'Continue' : `Grant access to continue${pendingCount() > 1 ? ` (${pendingCount()} left)` : ''}`}
      primaryDisabled={!canContinue()}
      onPrimary={props.onNext}
    >
      <Show
        when={!perms.loading}
        fallback={<div style={{ color: 'var(--safai-fg-2)' }}>Probing host permissions…</div>}
      >
        <div style={{ display: 'flex', 'flex-direction': 'column', gap: '14px' }}>
          <For each={perms() ?? []}>
            {(p) => {
              const persisted = () =>
                props.state.permissions.find((r) => r.kind === p.kind)?.status ?? null;
              const merged = (): PermissionStatus => effectiveStatus(p.kind, p.status);
              return (
                <PermissionRow
                  entry={p}
                  merged={merged()}
                  persisted={persisted()}
                  busy={busy() === p.kind}
                  onOpen={() => handleOpen(p.kind)}
                  onMark={(s) => handleVerdict(p.kind, s)}
                />
              );
            }}
          </For>
          <Show when={(perms() ?? []).length === 0}>
            <div
              style={{
                padding: '20px',
                'border-radius': '8px',
                background: 'oklch(0.20 0.02 255)',
                color: 'var(--safai-fg-2)',
                'font-size': '12px',
                'line-height': 1.5,
              }}
            >
              No OS-level permissions are needed to scan your home directory on
              this platform. Click continue to set your preferences.
            </div>
          </Show>
          <Show when={!canContinue() && (perms() ?? []).length > 0}>
            <div
              style={{
                padding: '12px 14px',
                'border-radius': '8px',
                background: 'color-mix(in oklab, oklch(0.70 0.17 25) 10%, var(--safai-bg-2))',
                border: '1px solid color-mix(in oklab, oklch(0.70 0.17 25) 40%, transparent)',
                color: 'var(--safai-fg-1)',
                'font-size': '12px',
                'line-height': 1.5,
                display: 'flex',
                'align-items': 'flex-start',
                gap: '10px',
              }}
            >
              <Icon name="shield" size={12} color="oklch(0.72 0.17 25)" />
              <span>
                Safai can't scan what it can't read. Every item above needs
                to be granted before I can continue. If you'd rather not
                grant access, close the app - there's nothing useful I can
                do without it.
              </span>
            </div>
          </Show>
        </div>
      </Show>
    </StepShell>
  );
}

function PermissionRow(props: {
  entry: PermissionStatusEntry;
  merged: PermissionStatus;
  persisted: PermissionStatus | null;
  busy: boolean;
  onOpen: () => void;
  onMark: (s: PermissionStatus) => void;
}) {
  const dot = () => {
    const map: Record<PermissionStatus, string> = {
      granted: 'oklch(0.78 0.14 140)',
      denied: 'oklch(0.70 0.17 25)',
      unknown: 'var(--safai-fg-3)',
    };
    return map[props.merged];
  };

  return (
    <div
      class="safai-card"
      style={{
        padding: '16px 18px',
        display: 'flex',
        'flex-direction': 'column',
        gap: '12px',
      }}
    >
      <div style={{ display: 'flex', 'align-items': 'flex-start', gap: '12px' }}>
        <div
          style={{
            width: '10px',
            height: '10px',
            'border-radius': '50%',
            background: dot(),
            'margin-top': '6px',
            'box-shadow': `0 0 8px ${dot()}`,
          }}
          title={props.merged}
        />
        <div style={{ flex: 1 }}>
          <div style={{ 'font-weight': 500, color: 'var(--safai-fg-0)', 'margin-bottom': '4px' }}>
            {labelForKind(props.entry.kind)}
          </div>
          <div style={{ color: 'var(--safai-fg-2)', 'font-size': '12px', 'line-height': 1.5 }}>
            {descriptionForKind(props.entry.kind)}
          </div>
        </div>
      </div>
      <div style={{ display: 'flex', gap: '8px', 'flex-wrap': 'wrap' }}>
        <Show when={props.entry.settingsUrl}>
          <button
            class="safai-btn safai-btn--primary"
            onClick={props.onOpen}
            disabled={props.busy}
          >
            <Icon name="globe" size={11} color="oklch(0.18 0.02 240)" />{' '}
            Open System Settings
          </button>
        </Show>
        <button
          class="safai-btn safai-btn--ghost"
          onClick={() => props.onMark('granted')}
          aria-pressed={props.persisted === 'granted'}
        >
          <Icon name="check" size={11} /> Granted
        </button>
        <button
          class="safai-btn safai-btn--ghost"
          onClick={() => props.onMark('denied')}
          aria-pressed={props.persisted === 'denied'}
          title="You'll need to grant this later from Settings before Safai can scan."
        >
          Decline
        </button>
      </div>
    </div>
  );
}

// step 3: prefs

const CATEGORY_LABELS: Record<IncludedCategory, string> = {
  'system-junk': 'System Junk',
  duplicates: 'Duplicates',
  'large-old': 'Large & Old',
  privacy: 'Privacy',
  'app-leftovers': 'App leftovers',
  trash: 'Trash',
};

function PrefsStep(props: {
  state: OnboardingState;
  onNext: () => void;
  onBack: () => void;
  onMutate: (s: OnboardingState) => void;
}) {
  const [prefs, setPrefs] = createSignal<Preferences>(props.state.prefs ?? defaultPreferences());

  const toggleCat = (c: IncludedCategory) => {
    setPrefs((p) => {
      const has = p.includedCategories.includes(c);
      return {
        ...p,
        includedCategories: has
          ? p.includedCategories.filter((x) => x !== c)
          : [...p.includedCategories, c],
      };
    });
  };

  const save = async () => {
    const merged = await saveOnboardingPrefs(prefs());
    props.onMutate(merged);
    props.onNext();
  };

  return (
    <StepShell
      step="prefs"
      title="How do you want me to run?"
      subtitle="Pick the knobs you care about - every one of these can be changed later from Settings."
      mood="happy"
      onBack={props.onBack}
      primaryLabel="Save and continue"
      onPrimary={save}
    >
      <div style={{ display: 'flex', 'flex-direction': 'column', gap: '22px' }}>
        <section>
          <SectionLabel text="Scan these categories" />
          <div style={{ display: 'grid', 'grid-template-columns': 'repeat(2, 1fr)', gap: '8px' }}>
            <For each={Object.keys(CATEGORY_LABELS) as IncludedCategory[]}>
              {(c) => (
                <CheckboxRow
                  label={CATEGORY_LABELS[c]}
                  checked={prefs().includedCategories.includes(c)}
                  onChange={() => toggleCat(c)}
                />
              )}
            </For>
          </div>
        </section>

        <section>
          <SectionLabel text="Large & Old thresholds" />
          <div style={{ display: 'grid', 'grid-template-columns': '1fr 1fr', gap: '12px' }}>
            <ThresholdInput
              label="Minimum size"
              value={prefs().largeMinBytes}
              hint={formatBytes(prefs().largeMinBytes)}
              min={1024 * 1024}
              step={1024 * 1024}
              onChange={(n) => setPrefs((p) => ({ ...p, largeMinBytes: n }))}
            />
            <ThresholdInput
              label="Days untouched"
              value={prefs().largeMinDaysIdle}
              hint={`${prefs().largeMinDaysIdle} days`}
              min={30}
              step={30}
              onChange={(n) => setPrefs((p) => ({ ...p, largeMinDaysIdle: n }))}
            />
          </div>
        </section>

        <section>
          <SectionLabel text="Behaviour" />
          <div style={{ display: 'flex', 'flex-direction': 'column', gap: '8px' }}>
            <CheckboxRow
              label="Run a scan automatically every time I open Safai"
              checked={prefs().autoScanOnLaunch}
              onChange={() =>
                setPrefs((p) => ({ ...p, autoScanOnLaunch: !p.autoScanOnLaunch }))
              }
            />
            <CheckboxRow
              label="Ask me before cleaning anything (recommended)"
              checked={prefs().confirmBeforeClean}
              onChange={() =>
                setPrefs((p) => ({ ...p, confirmBeforeClean: !p.confirmBeforeClean }))
              }
            />
          </div>
        </section>
      </div>
    </StepShell>
  );
}

function SectionLabel(props: { text: string }) {
  return (
    <div
      style={{
        'font-size': '11px',
        color: 'var(--safai-fg-3)',
        'letter-spacing': '0.1em',
        'text-transform': 'uppercase',
        'margin-bottom': '10px',
      }}
    >
      {props.text}
    </div>
  );
}

function CheckboxRow(props: { label: string; checked: boolean; onChange: () => void }) {
  return (
    <button
      class="safai-card safai-card--hover"
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
      onClick={props.onChange}
      aria-pressed={props.checked}
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
  hint: string;
  min: number;
  step: number;
  onChange: (n: number) => void;
}) {
  return (
    <label
      class="safai-card"
      style={{
        padding: '10px 14px',
        display: 'flex',
        'flex-direction': 'column',
        gap: '6px',
      }}
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

// step 4: ready

function ReadyStep(props: { onBack: () => void; onFinish: (runFirstScan: boolean) => void }) {
  const [starting, setStarting] = createSignal(false);
  return (
    <StepShell
      step="ready"
      title="All set"
      subtitle="Want me to run a first scan right now? I'll show you everything before anything gets touched."
      mood="zoom"
      onBack={props.onBack}
      primaryLabel={starting() ? 'Starting…' : 'Run my first scan'}
      primaryDisabled={starting()}
      onPrimary={async () => {
        setStarting(true);
        props.onFinish(true);
      }}
      secondary={{
        label: 'Skip, just open the app',
        onClick: () => props.onFinish(false),
      }}
    >
      <div
        style={{
          display: 'flex',
          'flex-direction': 'column',
          gap: '14px',
          color: 'var(--safai-fg-1)',
          'font-size': '13px',
          'line-height': 1.6,
        }}
      >
        <div>
          I'll walk your home directory, sum what's reclaimable, and present
          the results in the dashboard. Nothing is deleted during the scan.
        </div>
        <div
          style={{
            padding: '12px 14px',
            'border-radius': '8px',
            background: 'oklch(0.20 0.02 255)',
            border: '1px solid var(--safai-line)',
            color: 'var(--safai-fg-2)',
            'font-size': '12px',
          }}
        >
          <Icon name="shield" size={12} color="var(--safai-cyan)" /> Everything
          I remove is moved to Safai's own trash first. Restoring the last
          clean is one click away.
        </div>
      </div>
    </StepShell>
  );
}

// router entry points

export default function Onboarding(props: { step: OnboardingStep }) {
  const navigate = useNavigate();
  const [state, setState] = createSignal<OnboardingState | null>(null);

  onMount(async () => {
    const s = await getOnboardingState();
    setState(s);
  });

  const advanceTo = async (next: OnboardingStep) => {
    const s = await setOnboardingStep(next);
    setState(s);
    navigate(`/onboarding/${next}`);
  };

  const finish = async (runFirstScan: boolean) => {
    await completeOnboarding();
    if (runFirstScan) {
      navigate('/scanning');
      return;
    }
    navigate('/scan');
  };

  return (
    <Show when={state()} fallback={<OnboardingLoader />}>
      {(s) => {
        switch (props.step) {
          case 'welcome':
            return (
              <WelcomeStep onNext={() => advanceTo('permissions')} />
            );
          case 'permissions':
            return (
              <PermissionsStep
                state={s()}
                onNext={() => advanceTo('prefs')}
                onBack={() => advanceTo('welcome')}
                onMutate={setState}
              />
            );
          case 'prefs':
            return (
              <PrefsStep
                state={s()}
                onNext={() => advanceTo('ready')}
                onBack={() => advanceTo('permissions')}
                onMutate={setState}
              />
            );
          case 'ready':
            return (
              <ReadyStep
                onBack={() => advanceTo('prefs')}
                onFinish={finish}
              />
            );
          default:
            return <OnboardingLoader />;
        }
      }}
    </Show>
  );
}

function OnboardingLoader() {
  return (
    <div
      style={{
        flex: 1,
        display: 'flex',
        'align-items': 'center',
        'justify-content': 'center',
        'min-height': '100vh',
      }}
    >
      <Suds size={80} mood="sleepy" float />
    </div>
  );
}
