import { invalidateFilesystemScanCaches } from './scanCache';
import { invalidateTreemapCache } from './treemap';

export function invalidateFilesystemCaches(
  except?: string | string[],
): Promise<void> {
  invalidateFilesystemScanCaches(except);
  return invalidateTreemapCache().catch(() => undefined);
}

export function invalidateFilesystemCachesSoon(except?: string | string[]): void {
  void invalidateFilesystemCaches(except);
}
