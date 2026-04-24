// settings surface. mirrors rust commands::SettingsBundle +
// scheduler::SchedulerStatus. wire is camelCase + kebab serde enums
// (serde snapshots assert on every rust commit)

import { invoke, listen } from './ipc';
import {
  defaultPreferences,
  type Preferences,
  type ScheduleCadence,
} from './onboarding';

export interface SchedulerStatus {
  cadence: ScheduleCadence | null;
  lastRunAt: number | null;
  nextRunAt: number | null;
  secondsUntilNext: number | null;
}

export interface SettingsBundle {
  prefs: Preferences;
  telemetryOptIn: boolean;
  completedAt: number | null;
  lastScheduledAt: number | null;
  scheduler: SchedulerStatus;
  appVersion: string;
}

// rust scheduler emits this when a scheduled scan is due. main shell
// listens and calls startScan() in response
export const EVENT_SCHEDULER_FIRED = 'scheduler://fired';

export function defaultSettingsBundle(): SettingsBundle {
  return {
    prefs: defaultPreferences(),
    telemetryOptIn: false,
    completedAt: null,
    lastScheduledAt: null,
    scheduler: {
      cadence: null,
      lastRunAt: null,
      nextRunAt: null,
      secondsUntilNext: null,
    },
    appVersion: 'dev',
  };
}

export function getSettings(): Promise<SettingsBundle> {
  return invoke<SettingsBundle>('settings_get', undefined, defaultSettingsBundle);
}

export function updateSettings(
  prefs: Preferences,
  telemetryOptIn: boolean,
): Promise<SettingsBundle> {
  return invoke<SettingsBundle>(
    'settings_update',
    { prefs, telemetryOptIn },
    () => ({
      ...defaultSettingsBundle(),
      prefs,
      telemetryOptIn,
    }),
  );
}

export function resetPrefs(): Promise<SettingsBundle> {
  return invoke<SettingsBundle>(
    'settings_reset_prefs',
    undefined,
    defaultSettingsBundle,
  );
}

export function getSchedulerStatus(): Promise<SchedulerStatus> {
  return invoke<SchedulerStatus>('scheduler_status', undefined, () => ({
    cadence: null,
    lastRunAt: null,
    nextRunAt: null,
    secondsUntilNext: null,
  }));
}

export function nudgeScheduler(): Promise<boolean> {
  return invoke<boolean>('scheduler_nudge', undefined, () => true);
}

// subscribe to scheduler-fired events. handler usually kicks off a scan
export function onSchedulerFired(handler: () => void): Promise<() => void> {
  return listen<void>(EVENT_SCHEDULER_FIRED, handler);
}

// formatting helpers

export function labelForCadence(c: ScheduleCadence | null): string {
  switch (c) {
    case 'daily':
      return 'Every day';
    case 'weekly':
      return 'Every week';
    case 'monthly':
      return 'Every month';
    case null:
      return 'Off';
  }
}

// unix seconds -> short human string. null -> "-" so ui can interpolate
// without branching
export function formatTimestamp(t: number | null): string {
  if (t == null) return '-';
  const d = new Date(t * 1000);
  return d.toLocaleString();
}

// "in 3h 12m" / "any moment now" / "-". stays here (not format.ts)
// because the thresholds are scheduler-specific
export function formatRelativeSecs(s: number | null): string {
  if (s == null) return '-';
  if (s <= 0) return 'any moment now';
  if (s < 60) return `in ${Math.ceil(s)}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `in ${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) {
    const mm = m % 60;
    return mm > 0 ? `in ${h}h ${mm}m` : `in ${h}h`;
  }
  const days = Math.floor(h / 24);
  return `in ${days}d`;
}
