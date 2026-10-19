/**
 * The download cards, shared by the overview and the install page.
 *
 * macOS is the only platform with a build. Windows and Ubuntu get a card each anyway, saying
 * plainly that theirs is coming. Someone on Windows should learn that here, not by clicking a
 * button that hands them a disk image they cannot open.
 */

import {
  RELEASES_PAGE,
  findMacRelease,
  formatBytes,
  type Lookup,
  type MacFormat,
  type MacRelease,
} from "./release";

export type Os = "mac" | "windows" | "linux";

/** A first guess at the visitor's platform, so the right card is the one that stands out. */
export function detectOs(): Os {
  const hint = `${navigator.userAgent} ${navigator.platform ?? ""}`.toLowerCase();
  if (hint.includes("win")) return "windows";
  if (hint.includes("linux") || hint.includes("android")) return "linux";
  // macOS last as the default: it is the only platform with a build, so it is the least
  // misleading fallback when the guess fails.
  return "mac";
}

const FORMATS: Array<{ format: MacFormat; label: string; hint: string }> = [
  {
    format: "dmg",
    label: ".dmg",
    hint: "Recommended. Open the disk image and drag Notewise into Applications.",
  },
  {
    format: "zip",
    label: ".zip",
    hint: "Safari unpacks it to Notewise.app by itself. Move that into Applications.",
  },
  {
    format: "app",
    label: ".app",
    hint: "The bare app bundle, as .app.tar.gz. Double-click it to get Notewise.app.",
  },
];

const FORMAT_KEY = "notewise.format";

function savedFormat(): MacFormat {
  try {
    const value = localStorage.getItem(FORMAT_KEY);
    if (value === "dmg" || value === "zip" || value === "app") return value;
  } catch {
    // Private window: fall through to the default.
  }
  return "dmg";
}

function saveFormat(format: MacFormat): void {
  try {
    localStorage.setItem(FORMAT_KEY, format);
  } catch {
    // Not remembered; nothing else depends on it.
  }
}

const ICON = {
  apple:
    '<svg viewBox="0 0 24 24" class="h-7 w-7 fill-current" aria-hidden="true"><path d="M16.37 12.6c-.02-2.2 1.8-3.26 1.88-3.31-1.02-1.5-2.62-1.7-3.19-1.73-1.36-.14-2.65.8-3.34.8-.69 0-1.75-.78-2.88-.76-1.48.02-2.85.86-3.61 2.19-1.54 2.67-.39 6.62 1.11 8.79.73 1.06 1.6 2.25 2.75 2.2 1.1-.04 1.52-.71 2.85-.71 1.33 0 1.71.71 2.88.69 1.19-.02 1.94-1.08 2.66-2.14.84-1.23 1.19-2.42 1.21-2.48-.03-.01-2.32-.89-2.34-3.54ZM14.18 6.13c.61-.74 1.02-1.76.91-2.78-.88.04-1.94.59-2.57 1.32-.56.65-1.06 1.7-.93 2.7.98.08 1.98-.5 2.59-1.24Z"/></svg>',
  windows:
    '<svg viewBox="0 0 24 24" class="h-6 w-6 fill-current" aria-hidden="true"><path d="M3 5.1 10.4 4v7.2H3V5.1Zm0 13.8 7.4 1.1v-7.1H3v6Zm8.2 1.2L21 21.5v-8.6h-9.8v7.2Zm0-16.2v7.3H21V2.5L11.2 3.9Z"/></svg>',
  linux:
    '<svg viewBox="0 0 24 24" class="h-7 w-7" aria-hidden="true"><circle cx="12" cy="12" r="8.2" fill="none" stroke="currentColor" stroke-width="2.2"/><circle cx="12" cy="3.6" r="2.4" fill="currentColor"/><circle cx="4.7" cy="16.2" r="2.4" fill="currentColor"/><circle cx="19.3" cy="16.2" r="2.4" fill="currentColor"/></svg>',
  arrow:
    '<svg viewBox="0 0 20 20" class="h-5 w-5 fill-current" aria-hidden="true"><path d="M10 2.5a.9.9 0 0 1 .9.9v9.43l3.12-3.11a.9.9 0 1 1 1.27 1.27l-4.66 4.65a.9.9 0 0 1-1.27 0L4.7 10.99a.9.9 0 1 1 1.28-1.27l3.12 3.11V3.4a.9.9 0 0 1 .9-.9ZM3.5 16.2h13a.9.9 0 1 1 0 1.8h-13a.9.9 0 1 1 0-1.8Z"/></svg>',
};

function soonCard(os: Os, name: string, icon: string, files: string, current: Os, delay: number): string {
  const here = os === current;
  return `
    <article class="dl-card ${here ? "is-current" : ""}" data-tilt data-reveal="rise" style="--reveal-delay:${delay}ms">
      <div class="dl-sheen" aria-hidden="true"></div>
      <div class="flex items-center justify-between gap-3">
        <span class="text-ink-muted">${icon}</span>
        <span class="badge-soon">Coming soon</span>
      </div>
      <h3 class="mt-6 text-[22px] font-semibold tracking-[-0.02em]">${name}</h3>
      <p class="mt-1 text-[13.5px] text-ink-faint">${files}</p>
      ${here ? '<p class="mt-3 text-[12.5px] text-accent-soft">You’re on ' + name + ". It’s next.</p>" : ""}
      <button type="button" class="dl-button is-disabled mt-6" disabled aria-disabled="true">
        Coming soon
      </button>
      <p class="mt-4 text-[12.5px] leading-relaxed text-ink-faint">
        The engine and CLI already build here.
        <a class="text-ink-muted underline decoration-hairline underline-offset-2 hover:text-ink" href="${RELEASES_PAGE}" target="_blank" rel="noreferrer noopener">Watch releases</a>
        to hear when the app lands.
      </p>
    </article>`;
}

function macCard(current: Os, firstLaunchHref: string): string {
  const here = current === "mac";
  return `
    <article class="dl-card dl-card--mac ${here ? "is-current" : ""}" data-tilt data-reveal="rise">
      <div class="dl-sheen" aria-hidden="true"></div>
      <div class="flex items-center justify-between gap-3">
        <span class="text-ink">${ICON.apple}</span>
        <span class="badge-live" data-badge><span class="dot"></span>Available</span>
      </div>
      <h3 class="mt-6 text-[22px] font-semibold tracking-[-0.02em]">macOS</h3>
      <p class="mt-1 text-[13.5px] text-ink-faint">Apple silicon · macOS 13 or later</p>

      <a id="mac-download" class="dl-button is-primary mt-6" href="${RELEASES_PAGE}" rel="noopener">
        ${ICON.arrow}
        <span data-label>Download for Mac</span>
        <span data-meta class="dl-meta">.dmg</span>
      </a>

      <div class="fmt-group mt-4" role="radiogroup" aria-label="File format">
        <span class="fmt-pill" aria-hidden="true"></span>
        ${FORMATS.map(
          ({ format, label }) =>
            `<button type="button" class="fmt" role="radio" data-format="${format}" aria-checked="false">${label}</button>`,
        ).join("")}
      </div>
      <p data-hint class="mt-3 min-h-[2.6em] text-[12.5px] leading-relaxed text-ink-muted"></p>

      <p data-status class="mt-3 border-t border-hairline pt-3 text-[12px] leading-relaxed text-ink-faint">
        Checking the latest release…
      </p>
      <p class="mt-2 text-[12px] leading-relaxed text-ink-faint">
        Signed by the project, not by Apple, so the first launch needs
        <a class="text-ink-muted underline decoration-hairline underline-offset-2 hover:text-ink" href="${firstLaunchHref}">Open&nbsp;Anyway</a>.
      </p>
    </article>`;
}

/**
 * Render the three cards into `host` and resolve the Mac button against the real release.
 *
 * `sourceHref` is where "build it yourself" lives from the calling page; `firstLaunchHref` is the
 * Open Anyway walkthrough. Both are relative, and differ between the two pages.
 */
export function mountDownloads(
  host: HTMLElement,
  options: { sourceHref: string; firstLaunchHref: string },
): void {
  const current = detectOs();
  host.innerHTML = [
    macCard(current, options.firstLaunchHref),
    soonCard("windows", "Windows", ICON.windows, "Installer · .msi and .exe", current, 90),
    soonCard("linux", "Ubuntu", ICON.linux, "Linux · .deb and AppImage", current, 180),
  ].join("");

  const card = host.querySelector<HTMLElement>(".dl-card--mac");
  if (!card) return;

  const button = card.querySelector<HTMLAnchorElement>("#mac-download");
  const label = card.querySelector<HTMLElement>("[data-label]");
  const meta = card.querySelector<HTMLElement>("[data-meta]");
  const hint = card.querySelector<HTMLElement>("[data-hint]");
  const status = card.querySelector<HTMLElement>("[data-status]");
  const chips = [...card.querySelectorAll<HTMLButtonElement>(".fmt")];
  if (!button || !label || !meta || !hint || !status) return;

  let release: MacRelease | null = null;
  let format = savedFormat();

  const paint = () => {
    for (const chip of chips) {
      const chipFormat = chip.dataset.format as MacFormat;
      chip.setAttribute("aria-checked", String(chipFormat === format));
      // A format this release does not carry is shown but cannot be picked: an older release
      // may only have the dmg, and pretending otherwise would send someone to a 404.
      const missing = release !== null && !release.assets[chipFormat];
      chip.disabled = missing;
      chip.title = missing ? "Not in this release" : "";
    }

    // The pill slides under whichever format is picked.
    const picked = Math.max(0, FORMATS.findIndex((entry) => entry.format === format));
    card.style.setProperty("--fmt", String(picked));

    const choice = FORMATS[picked];
    hint.textContent = choice.hint;

    const asset = release?.assets[format];
    if (asset) {
      button.href = asset.url;
      meta.textContent = `${choice.label} · ${formatBytes(asset.bytes)}`;
    } else {
      meta.textContent = choice.label;
    }
  };

  for (const chip of chips) {
    chip.addEventListener("click", () => {
      format = chip.dataset.format as MacFormat;
      saveFormat(format);
      paint();
    });
  }

  paint();

  findMacRelease().then((lookup: Lookup) => {
    if (lookup.kind === "found") {
      release = lookup.release;
      // A remembered format the release does not have falls back to the dmg, which every Mac
      // release carries.
      if (!release.assets[format]) format = release.assets.dmg ? "dmg" : (Object.keys(release.assets)[0] as MacFormat);
      const kind = release.prerelease ? "Release candidate" : "Latest release";
      status.innerHTML = `${kind} <a class="text-ink-muted underline decoration-hairline underline-offset-2 hover:text-ink" href="${release.page}" target="_blank" rel="noreferrer noopener">${release.tag}</a> · straight from GitHub Releases.`;
      paint();
      return;
    }

    if (lookup.kind === "none") {
      // GitHub answered: there is no Mac build yet. The button stops promising a download.
      button.href = options.sourceHref;
      label.textContent = "Build it from source";
      meta.textContent = "~10 min";
      status.textContent =
        "The first macOS build has not been published yet. Until it is, the whole app builds from source.";
      card.classList.add("is-pending");
      const badge = card.querySelector<HTMLElement>("[data-badge]");
      if (badge) badge.innerHTML = '<span class="dot"></span>First build soon';
      return;
    }

    // Could not ask. The Releases page lists the same files, so the button still leads to them.
    status.textContent = "Couldn’t reach GitHub just now. The button opens the Releases page, which lists every file.";
  });
}
