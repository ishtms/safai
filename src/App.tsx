import { createEffect, createResource, createSignal, For, onCleanup, onMount, Show, type JSX } from 'solid-js';
import {
  Router,
  Route,
  Navigate,
  useLocation,
  useNavigate,
  type RouteSectionProps,
} from '@solidjs/router';
import { SafaiWindow } from './components/SafaiWindow';
import { SafaiSidebar } from './components/SafaiSidebar';
import { Placeholder } from './screens/Placeholder';
import SmartScan from './screens/SmartScan';
import SmartScanRunning from './screens/SmartScanRunning';
import Junk from './screens/Junk';
import Disk from './screens/Disk';
import Duplicates from './screens/Duplicates';
import LargeOld from './screens/LargeOld';
import Privacy from './screens/Privacy';
import Startup from './screens/Startup';
import Memory from './screens/Memory';
import Activity from './screens/Activity';
import Malware from './screens/Malware';
import Onboarding from './screens/Onboarding';
import Settings from './screens/Settings';
import { detectOS } from './lib/platform';
import { NAV, FALLBACK_PATH, type NavItem } from './lib/nav';
import type { OS } from './components/OSChip';
import { getOnboardingState, type OnboardingStep } from './lib/onboarding';
import { onSchedulerFired } from './lib/settings';
import { Suds } from './components/Suds';
import { UpdateBanner } from './components/UpdateBanner';

// shells

function Shell(props: RouteSectionProps): JSX.Element {
  const [os, setOS] = createSignal<OS>('mac');
  const navigate = useNavigate();
  onMount(async () => setOS(await detectOS()));

  // subscribe to scheduler-fired events once per shell mount. rust
  // scheduler emits when cadence elapses, we route to /scanning and let
  // the scanning screen attach to the single active smart-scan handle.
  onMount(async () => {
    const unlisten = await onSchedulerFired(async () => {
      navigate('/scanning');
    });
    onCleanup(unlisten);
  });

  return (
    <OnboardingGate>
      <SafaiWindow os={os()}>
        <SafaiSidebar />
        {props.children}
        <UpdateBanner />
      </SafaiWindow>
    </OnboardingGate>
  );
}

// cold-start gate. pulls persisted onboarding state, redirects into
// onboarding if incomplete. gating at shell level means screens never
// render until onboarding is done
function OnboardingGate(props: { children: JSX.Element }): JSX.Element {
  const [state] = createResource(getOnboardingState);
  const navigate = useNavigate();
  const location = useLocation();
  const [ready, setReady] = createSignal(false);

  // once state resolves, figure out where we belong. runs outside the
  // render phase so navigate() doesn't mutate router state mid-build
  createEffect(() => {
    const s = state();
    if (!s) return;
    if (s.completedAt == null && !location.pathname.startsWith('/onboarding')) {
      const resume: OnboardingStep = ['welcome', 'permissions', 'prefs', 'ready'].includes(
        s.lastStep,
      )
        ? (s.lastStep as OnboardingStep)
        : 'welcome';
      navigate(`/onboarding/${resume}`, { replace: true });
    }
    setReady(true);
  });

  return (
    <Show
      when={ready()}
      fallback={
        <div
          style={{
            display: 'flex',
            'align-items': 'center',
            'justify-content': 'center',
            height: '100vh',
            background: 'var(--safai-bg-0)',
          }}
        >
          <Suds size={64} mood="sleepy" float />
        </div>
      }
    >
      {props.children}
    </Show>
  );
}

// onboarding route tree lives outside main shell so it owns the full
// window (no sidebar, no toolbar chrome)
function OnboardingShell(props: { children: JSX.Element }): JSX.Element {
  const [os, setOS] = createSignal<OS>('mac');
  onMount(async () => setOS(await detectOS()));
  return (
    <SafaiWindow os={os()} showOSChip>
      {props.children}
    </SafaiWindow>
  );
}

// paths with real screens. anything else falls through to Placeholder
// so features can ship one at a time
const REAL_SCREENS: Record<string, () => JSX.Element> = {
  '/scan': SmartScan,
  '/junk': Junk,
  '/disk': Disk,
  '/dupes': Duplicates,
  '/large': LargeOld,
  '/privacy': Privacy,
  '/startup': Startup,
  '/memory': Memory,
  '/activity': Activity,
  '/malware': Malware,
  '/settings': Settings,
};

const ALL_ITEMS: NavItem[] = NAV.flatMap((g) => g.items);

export default function App() {
  return (
    <Router>
      {/* onboarding tree lives outside main shell, owns the full window */}
      <Route path="/onboarding/welcome" component={() => <OnboardingShell><Onboarding step="welcome" /></OnboardingShell>} />
      <Route path="/onboarding/permissions" component={() => <OnboardingShell><Onboarding step="permissions" /></OnboardingShell>} />
      <Route path="/onboarding/prefs" component={() => <OnboardingShell><Onboarding step="prefs" /></OnboardingShell>} />
      <Route path="/onboarding/ready" component={() => <OnboardingShell><Onboarding step="ready" /></OnboardingShell>} />
      <Route path="/onboarding" component={() => <Navigate href="/onboarding/welcome" />} />
      <Route path="/" component={Shell}>
        <For each={ALL_ITEMS}>
          {(item) => (
            <Route path={item.path} component={REAL_SCREENS[item.path] ?? Placeholder} />
          )}
        </For>
        {/* off-nav route for the streaming scan screen. entered via
            dashboard rescan button, exited on stop/done */}
        <Route path="/scanning" component={SmartScanRunning} />
        <Route path="*" component={() => <Navigate href={FALLBACK_PATH} />} />
      </Route>
    </Router>
  );
}
