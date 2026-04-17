// ts mirror of src-tauri/src/onboarding. rust stamps every save with a
// schema version. wire is camelCase + kebab serde enums (serde tests
// assert it). add fields on both sides or break the wire

import { invoke } from './ipc';

export type OnboardingStep =
  | 'welcome'
  | 'permissions'
  | 'prefs'
  | 'ready'
  | 'done';

export type ScheduleCadence = 'daily' | 'weekly' | 'monthly';

export type IncludedCategory =
  | 'system-junk'
  | 'duplicates'
  | 'large-old'
  | 'privacy'
  | 'app-leftovers'
  | 'trash';

export type PermissionKind =
  | 'mac-full-disk-access'
  | 'mac-files-and-folders'
  | 'linux-home-acknowledged'
  | 'windows-home-acknowledged';

export type PermissionStatus = 'granted' | 'denied' | 'unknown';

export interface PermissionRecord {
  kind: PermissionKind;
  status: PermissionStatus;
  answeredAt: number | null;
}

export interface Preferences {
  autoScanOnLaunch: boolean;
  scheduledScan: ScheduleCadence | null;
  includedCategories: IncludedCategory[];
  largeMinBytes: number;
  largeMinDaysIdle: number;
  confirmBeforeClean: boolean;
}

export interface OnboardingState {
  version: number;
  completedAt: number | null;
  lastStep: OnboardingStep;
  permissions: PermissionRecord[];
  prefs: Preferences;
  telemetryOptIn: boolean;
}

export interface PermissionStatusEntry {
  kind: PermissionKind;
  status: PermissionStatus;
  settingsUrl: string | null;
}

// matches rust OnboardingState::default(). fallback outside tauri
// (plain pnpm dev) so onboarding screens still render
export function defaultState(): OnboardingState {
  return {
    version: 1,
    completedAt: null,
    lastStep: 'welcome',
    permissions: [],
    prefs: defaultPreferences(),
    telemetryOptIn: false,
  };
}

export function defaultPreferences(): Preferences {
  return {
    autoScanOnLaunch: false,
    scheduledScan: null,
    includedCategories: [
      'system-junk',
      'duplicates',
      'large-old',
      'privacy',
      'app-leftovers',
      'trash',
    ],
    largeMinBytes: 50 * 1024 * 1024,
    largeMinDaysIdle: 180,
    confirmBeforeClean: true,
  };
}

export function getOnboardingState(): Promise<OnboardingState> {
  return invoke<OnboardingState>('onboarding_state', undefined, defaultState);
}

export function saveOnboardingPrefs(prefs: Preferences): Promise<OnboardingState> {
  return invoke<OnboardingState>(
    'onboarding_save_prefs',
    { prefs },
    () => ({ ...defaultState(), prefs }),
  );
}

export function setOnboardingStep(step: OnboardingStep): Promise<OnboardingState> {
  return invoke<OnboardingState>(
    'onboarding_set_step',
    { step },
    () => ({ ...defaultState(), lastStep: step }),
  );
}

export function recordPermission(
  kind: PermissionKind,
  status: PermissionStatus,
): Promise<OnboardingState> {
  return invoke<OnboardingState>(
    'onboarding_record_permission',
    { kind, status },
    () => {
      const s = defaultState();
      s.permissions = [{ kind, status, answeredAt: Date.now() / 1000 }];
      return s;
    },
  );
}

export function setTelemetryOptIn(optIn: boolean): Promise<OnboardingState> {
  return invoke<OnboardingState>(
    'onboarding_set_telemetry',
    { optIn },
    () => ({ ...defaultState(), telemetryOptIn: optIn }),
  );
}

export function completeOnboarding(): Promise<OnboardingState> {
  return invoke<OnboardingState>('onboarding_complete', undefined, () => ({
    ...defaultState(),
    completedAt: Math.floor(Date.now() / 1000),
    lastStep: 'done',
  }));
}

export function resetOnboarding(): Promise<void> {
  return invoke<void>('onboarding_reset', undefined, () => undefined);
}

export function fetchPermissionStatus(): Promise<PermissionStatusEntry[]> {
  return invoke<PermissionStatusEntry[]>(
    'onboarding_permission_status',
    undefined,
    () => [],
  );
}

export function openPermissionSettings(kind: PermissionKind): Promise<void> {
  return invoke<void>('open_permission_settings', { kind }, () => undefined);
}

export const ONBOARDING_STEPS: OnboardingStep[] = [
  'welcome',
  'permissions',
  'prefs',
  'ready',
];

export function stepIndex(step: OnboardingStep): number {
  // `done` sits after `ready` but isn't a visible step
  const i = ONBOARDING_STEPS.indexOf(step);
  return i === -1 ? ONBOARDING_STEPS.length : i;
}

export function labelForKind(kind: PermissionKind): string {
  switch (kind) {
    case 'mac-full-disk-access':
      return 'Full Disk Access';
    case 'mac-files-and-folders':
      return 'Files & Folders';
    case 'linux-home-acknowledged':
    case 'windows-home-acknowledged':
      return 'Home folder access';
  }
}

export function descriptionForKind(kind: PermissionKind): string {
  switch (kind) {
    case 'mac-full-disk-access':
      return "Lets Safai see your Mail, Safari, and app containers so the Privacy cleaner knows what's cached. Nothing is uploaded.";
    case 'mac-files-and-folders':
      return 'Grants access to Desktop, Documents, and Downloads so Large & Old can find big forgotten files.';
    case 'linux-home-acknowledged':
      return "Safai scans files under your home directory. Nothing is uploaded and nothing is deleted without your explicit confirmation.";
    case 'windows-home-acknowledged':
      return "Safai scans files under your user folder. Nothing is uploaded and nothing is deleted without your explicit confirmation.";
  }
}
