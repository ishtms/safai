// ts mirror of src-tauri/src/scanner/startup. camelCase structs, kebab
// serde ids. rust test report_wire_shape_is_camel_case guards the
// contract, update both sides when renaming

import { invoke } from './ipc';

export type StartupSource =
  | 'linux-autostart'
  | 'linux-systemd-user'
  | 'mac-launch-agent-user'
  | 'mac-launch-agent-system'
  | 'mac-launch-daemon'
  | 'windows-startup-folder'
  | 'windows-run-user'
  | 'windows-run-machine';

export type StartupImpact = 'low' | 'medium' | 'high';

export interface StartupItem {
  id: string;
  name: string;
  description: string;
  command: string;
  source: StartupSource;
  path: string;
  enabled: boolean;
  isUser: boolean;
  icon: string;
  impact: StartupImpact;
}

export interface StartupReport {
  items: StartupItem[];
  baselineSeconds: number;
  durationMs: number;
  scannedAt: number;
  platform: 'mac' | 'linux' | 'windows';
}

export interface ToggleResult {
  id: string;
  enabled: boolean;
}

/** sources we can't toggle without admin rights */
const READ_ONLY_SOURCES: ReadonlySet<StartupSource> = new Set([
  'mac-launch-agent-system',
  'mac-launch-daemon',
  'windows-run-machine',
]);

export function isToggleable(source: StartupSource): boolean {
  return !READ_ONLY_SOURCES.has(source);
}

// boot seconds added per impact tier. stays roughly in sync with rust
// StartupImpact::boot_seconds. only used for the sparkline, not
// load-bearing
export const BOOT_SECONDS_PER_IMPACT: Record<StartupImpact, number> = {
  low: 0.3,
  medium: 0.9,
  high: 2.4,
};

/** before/after boot seconds based on currently enabled items */
export function estimateBootSeconds(
  report: StartupReport,
  overrides: Map<string, boolean> = new Map(),
): { before: number; after: number } {
  let before = report.baselineSeconds;
  let after = report.baselineSeconds;
  for (const item of report.items) {
    const weight = BOOT_SECONDS_PER_IMPACT[item.impact];
    if (item.enabled) before += weight;
    const effective = overrides.has(item.id) ? overrides.get(item.id)! : item.enabled;
    if (effective) after += weight;
  }
  return { before, after };
}

export function startupScan(): Promise<StartupReport> {
  return invoke<StartupReport>('startup_scan', undefined, mockStartupReport);
}

export function startupToggle(
  source: StartupSource,
  path: string,
  enabled: boolean,
): Promise<ToggleResult> {
  return invoke<ToggleResult>('startup_toggle', { source, path, enabled }, async () => ({
    id: `${source}::${path.split('/').pop() ?? ''}`,
    enabled,
  }));
}

/** group items by source for the stacked card layout */
export function groupBySource(
  report: StartupReport,
): { source: StartupSource; items: StartupItem[] }[] {
  const byKey = new Map<StartupSource, StartupItem[]>();
  for (const it of report.items) {
    const list = byKey.get(it.source) ?? [];
    list.push(it);
    byKey.set(it.source, list);
  }
  // stable display order, user-scope first
  const order: StartupSource[] = [
    'linux-autostart',
    'linux-systemd-user',
    'mac-launch-agent-user',
    'windows-startup-folder',
    'windows-run-user',
    'mac-launch-agent-system',
    'mac-launch-daemon',
    'windows-run-machine',
  ];
  const out: { source: StartupSource; items: StartupItem[] }[] = [];
  for (const s of order) {
    const items = byKey.get(s);
    if (items && items.length > 0) out.push({ source: s, items });
  }
  return out;
}

export function labelForSource(s: StartupSource): string {
  switch (s) {
    case 'linux-autostart':
      return 'Autostart (.desktop)';
    case 'linux-systemd-user':
      return 'systemd user units';
    case 'mac-launch-agent-user':
      return 'Launch Agents (user)';
    case 'mac-launch-agent-system':
      return 'Launch Agents (system-wide)';
    case 'mac-launch-daemon':
      return 'Launch Daemons (system)';
    case 'windows-startup-folder':
      return 'Startup folder';
    case 'windows-run-user':
      return 'Run registry (user)';
    case 'windows-run-machine':
      return 'Run registry (machine)';
  }
}

function mockStartupReport(): StartupReport {
  const now = Math.floor(Date.now() / 1000);
  const items: StartupItem[] = [
    {
      id: 'linux-autostart::slack',
      name: 'Slack',
      description: 'Team messaging',
      command: '/opt/Slack/slack -u',
      source: 'linux-autostart',
      path: '~/.config/autostart/slack.desktop',
      enabled: true,
      isUser: true,
      icon: 'power',
      impact: 'high',
    },
    {
      id: 'linux-autostart::docker',
      name: 'Docker Desktop',
      description: 'Container runtime',
      command: '/usr/bin/docker-desktop',
      source: 'linux-autostart',
      path: '~/.config/autostart/docker.desktop',
      enabled: true,
      isUser: true,
      icon: 'power',
      impact: 'high',
    },
    {
      id: 'linux-systemd-user::syncthing.service',
      name: 'syncthing.service',
      description: 'Continuous file sync',
      command: '/usr/bin/syncthing',
      source: 'linux-systemd-user',
      path: '~/.config/systemd/user/syncthing.service',
      enabled: false,
      isUser: true,
      icon: 'pulse',
      impact: 'medium',
    },
    {
      id: 'linux-autostart::redshift',
      name: 'Redshift',
      description: 'Adjusts screen colour temperature',
      command: '/usr/bin/redshift-gtk',
      source: 'linux-autostart',
      path: '~/.config/autostart/redshift-gtk.desktop',
      enabled: true,
      isUser: true,
      icon: 'power',
      impact: 'low',
    },
  ];
  return {
    items,
    baselineSeconds: 8,
    durationMs: 18,
    scannedAt: now,
    platform: 'linux',
  };
}
