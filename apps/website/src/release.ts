/**
 * Finds the macOS and Windows builds to download.
 *
 * # Why the release is looked up rather than linked
 *
 * The files carry the version in their names (`Notewise_0.1.0_aarch64.dmg`), so no fixed URL
 * points at "the current one". GitHub's `releases/latest/download/<name>` would need a stable
 * name, and it also skips pre-releases, which is what a release candidate is published as. A
 * button that 404s until the first final release would be worse than no button.
 *
 * So the page asks the GitHub API once and reads the real file names and sizes. If the API is
 * unreachable or rate-limited (60 requests an hour per address, unauthenticated), the button still
 * works: it falls back to the Releases page, where the same files are listed.
 */

const REPO = "sriksven/Notewise";

export const RELEASES_PAGE = `https://github.com/${REPO}/releases`;

/** The three ways the same signed app is shipped. `release.yml` makes all three. */
export type MacFormat = "dmg" | "zip" | "app";

export interface MacAsset {
  format: MacFormat;
  name: string;
  url: string;
  bytes: number;
}

export interface MacRelease {
  tag: string;
  prerelease: boolean;
  page: string;
  assets: Partial<Record<MacFormat, MacAsset>>;
}

/** The Windows installer, `Notewise_<version>_x64-setup.exe`. `release.yml` makes one. */
export interface WindowsRelease {
  tag: string;
  prerelease: boolean;
  page: string;
  installer: { name: string; url: string; bytes: number };
}

/**
 * What the lookup found. A `null` platform means GitHub answered and there is no build for it
 * yet; `unknown` means GitHub could not be asked. The two must not be confused: one is a fact
 * about the project, the other about the visitor's network.
 */
export type Lookup =
  | { kind: "found"; mac: MacRelease | null; windows: WindowsRelease | null }
  | { kind: "unknown" };

interface ApiAsset {
  name: string;
  browser_download_url: string;
  size: number;
}

interface ApiRelease {
  tag_name: string;
  html_url: string;
  draft: boolean;
  prerelease: boolean;
  assets: ApiAsset[];
}

/** Longest suffix first: `.app.tar.gz` and `.app.zip` must not be read as something shorter. */
const SUFFIXES: Array<[MacFormat, string]> = [
  ["app", ".app.tar.gz"],
  ["zip", ".app.zip"],
  ["dmg", ".dmg"],
];

function formatOf(name: string): MacFormat | null {
  const lower = name.toLowerCase();
  for (const [format, suffix] of SUFFIXES) {
    if (lower.endsWith(suffix)) return format;
  }
  return null;
}

function macAssets(release: ApiRelease): MacRelease["assets"] {
  const found: MacRelease["assets"] = {};
  for (const asset of release.assets) {
    const format = formatOf(asset.name);
    if (!format) continue;
    // Apple silicon first if a release ever carries more than one architecture: it is the only
    // one the release workflow builds today, and the one nearly every Mac sold since 2021 has.
    const current = found[format];
    if (current && current.name.includes("aarch64")) continue;
    found[format] = {
      format,
      name: asset.name,
      url: asset.browser_download_url,
      bytes: asset.size,
    };
  }
  return found;
}

/**
 * The release to offer for one platform: the newest final release with a build for it, or failing
 * that the newest release candidate. Final first because a candidate is, by definition, not yet
 * the one we recommend, but a candidate is better than telling someone there is nothing.
 *
 * Per platform, because the platforms do not arrive together: rc.2 had a Mac build and no Windows
 * one, and a release with only a Windows fix would be the reverse.
 */
function newest<T>(releases: ApiRelease[], buildOf: (release: ApiRelease) => T | null) {
  const usable = releases
    .filter((release) => !release.draft)
    .map((release) => ({ release, build: buildOf(release) }))
    .filter((entry): entry is { release: ApiRelease; build: T } => entry.build !== null);
  return usable.find(({ release }) => !release.prerelease) ?? usable[0] ?? null;
}

export function pickRelease(releases: ApiRelease[]): MacRelease | null {
  const chosen = newest(releases, (release) => {
    const assets = macAssets(release);
    return Object.keys(assets).length > 0 ? assets : null;
  });
  if (!chosen) return null;

  return {
    tag: chosen.release.tag_name,
    prerelease: chosen.release.prerelease,
    page: chosen.release.html_url,
    assets: chosen.build,
  };
}

export function pickWindowsRelease(releases: ApiRelease[]): WindowsRelease | null {
  const chosen = newest(releases, (release) => {
    const asset = release.assets.find((a) => a.name.toLowerCase().endsWith("-setup.exe"));
    return asset ? { name: asset.name, url: asset.browser_download_url, bytes: asset.size } : null;
  });
  if (!chosen) return null;

  return {
    tag: chosen.release.tag_name,
    prerelease: chosen.release.prerelease,
    page: chosen.release.html_url,
    installer: chosen.build,
  };
}

const CACHE_KEY = "notewise.release.v2";
const CACHE_MS = 10 * 60 * 1000;

/**
 * Remembered for ten minutes per visitor, so moving between the two pages does not spend two of
 * the sixty requests an hour. Storage can throw in a private window; the lookup then just runs.
 */
function cached(): Lookup | null {
  try {
    const raw = sessionStorage.getItem(CACHE_KEY);
    if (!raw) return null;
    const entry = JSON.parse(raw) as { at: number; lookup: Lookup };
    return Date.now() - entry.at < CACHE_MS ? entry.lookup : null;
  } catch {
    return null;
  }
}

function remember(lookup: Lookup): void {
  try {
    sessionStorage.setItem(CACHE_KEY, JSON.stringify({ at: Date.now(), lookup }));
  } catch {
    // Nothing to do: the next page view asks again.
  }
}

let pending: Promise<Lookup> | null = null;

export function findReleases(): Promise<Lookup> {
  const hit = cached();
  if (hit) return Promise.resolve(hit);

  pending ??= (async (): Promise<Lookup> => {
    try {
      const response = await fetch(`https://api.github.com/repos/${REPO}/releases?per_page=20`, {
        headers: { Accept: "application/vnd.github+json" },
      });
      if (!response.ok) return { kind: "unknown" };
      const releases = (await response.json()) as ApiRelease[];
      const lookup: Lookup = {
        kind: "found",
        mac: pickRelease(releases),
        windows: pickWindowsRelease(releases),
      };
      remember(lookup);
      return lookup;
    } catch {
      return { kind: "unknown" };
    }
  })();

  return pending;
}

export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}
