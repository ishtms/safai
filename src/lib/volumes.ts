// ts mirror of src-tauri/src/volumes/types.rs. rust owns the wire,
// test serializes_as_camelcase_with_kebab_kind guards drift

import { invoke } from './ipc';

export type VolumeKind = 'ssd' | 'hdd' | 'removable' | 'network' | 'unknown';

export interface Volume {
  name: string;
  mountPoint: string;
  totalBytes: number;
  freeBytes: number;
  usedBytes: number;
  fileSystem: string;
  kind: VolumeKind;
  isRemovable: boolean;
  isPrimary: boolean;
}

// per-volume telemetry from the rust backend. outside tauri returns a
// small well-formed mock so screens render without hanging
export function listVolumes(): Promise<Volume[]> {
  return invoke<Volume[]>('list_volumes', undefined, () => [
    {
      name: 'Demo Disk',
      mountPoint: '/',
      totalBytes: 1_000 * 1024 ** 3,
      freeBytes: 287 * 1024 ** 3,
      usedBytes: 713 * 1024 ** 3,
      fileSystem: 'demo',
      kind: 'ssd',
      isRemovable: false,
      isPrimary: true,
    },
  ]);
}

/** volume to highlight in sidebar footer + smart scan hero */
export function pickPrimary(volumes: Volume[]): Volume | null {
  return volumes.find((v) => v.isPrimary) ?? volumes[0] ?? null;
}
