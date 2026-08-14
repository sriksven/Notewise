/** Bytes as GB/MB. Model sizes span 77 MB to 3 GB, so one unit does not serve both. */
export function size(bytes: number): string {
  return bytes >= 1_000_000_000
    ? `${(bytes / 1_000_000_000).toFixed(1)} GB`
    : `${Math.round(bytes / 1_000_000)} MB`;
}
