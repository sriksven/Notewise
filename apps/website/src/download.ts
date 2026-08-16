/**
 * The install page.
 *
 * # Why there are no download buttons
 *
 * There is no release channel: no signed builds, no installers, no auto-update. Three big
 * buttons pointing at binaries that do not exist would be the single most dishonest thing on
 * this site, and the first click would prove it.
 *
 * So the page states each platform's real status and gives commands that work today. The
 * buttons that exist all do something: copy a command, switch platform, open the repository.
 */

import "./style.css";

const REDUCED = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

type Os = "mac" | "windows" | "linux";

interface Platform {
  os: Os;
  name: string;
  /** How much of the product works here, stated before any command is shown. */
  status: "works" | "partial" | "engine-only";
  summary: string;
}

const PLATFORMS: Platform[] = [
  {
    os: "mac",
    name: "macOS",
    status: "works",
    summary:
      "Apple silicon or Intel. The desktop app, recording, GPU-accelerated transcription — everything except system audio.",
  },
  {
    os: "windows",
    name: "Windows",
    status: "engine-only",
    summary:
      "The engine, CLI and web dashboard build and run. The desktop shell has not been built for Windows yet.",
  },
  {
    os: "linux",
    name: "Linux / Ubuntu",
    status: "engine-only",
    summary:
      "Same as Windows: the engine, CLI and dashboard work. The desktop shell is a later phase.",
  },
];

const BADGE: Record<Platform["status"], { label: string; classes: string }> = {
  works: { label: "Supported", classes: "bg-accent/15 text-accent-soft" },
  partial: { label: "Partial", classes: "bg-record/15 text-record" },
  "engine-only": { label: "Engine only", classes: "bg-white/10 text-ink-muted" },
};

interface Step {
  title: string;
  body?: string;
  command?: string;
}

/** Shared by every platform: the toolchain and the checkout. */
function common(pkg: string): Step[] {
  return [
    {
      title: "Install the toolchain",
      body: "Rust, plus cmake for the Whisper build and Node for the frontend.",
      command: pkg,
    },
    {
      title: "Clone the repository",
      command: "git clone https://github.com/sriksven/Notewise.git\ncd Notewise",
    },
    {
      title: "Build and test the engine",
      body: "Around 1,100 tests. If this passes, the engine is sound on your machine.",
      command: "cargo build --workspace\ncargo test --workspace",
    },
  ];
}

const STEPS: Record<Os, Step[]> = {
  mac: [
    ...common(
      "# Rust\ncurl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh\n\n# cmake and node\nbrew install cmake node",
    ),
    {
      title: "Run the engine with recording enabled",
      body: "The record and whisper features pull the platform audio SDK and a cmake build of whisper.cpp, so this first build is slow.",
      command: "cargo run -p notewise-cli --features record,whisper -- serve",
    },
    {
      title: "Start the desktop app",
      body: "In a second terminal. It opens on http://localhost:1420 against the engine you just started.",
      command: "cd apps/desktop\nnpm install\nnpm run dev",
    },
  ],
  windows: [
    ...common(
      "# In PowerShell — Rust, cmake, Node, and the MSVC build tools\nwinget install Rustlang.Rustup\nwinget install Kitware.CMake\nwinget install OpenJS.NodeJS\nwinget install Microsoft.VisualStudio.2022.BuildTools",
    ),
    {
      title: "Run the engine",
      body: "Microphone capture is untested on Windows. Without the record feature you get a full engine for importing audio, searching, asking questions and running the agent.",
      command: "cargo run -p notewise-cli -- serve",
    },
    {
      title: "Use the web dashboard",
      body: "There is no Windows desktop shell yet. The dashboard is a read-only view of the same workspace, in a browser.",
      command: "cd apps/web-dashboard\nnpm install\nnpm run dev",
    },
  ],
  linux: [
    ...common(
      "# Debian / Ubuntu\nsudo apt update\nsudo apt install -y build-essential cmake pkg-config libssl-dev nodejs npm\ncurl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh",
    ),
    {
      title: "Run the engine",
      body: "Microphone capture through ALSA is untested. Without the record feature you get a full engine for importing audio, searching, asking questions and running the agent.",
      command: "cargo run -p notewise-cli -- serve",
    },
    {
      title: "Use the web dashboard",
      body: "There is no Linux desktop shell yet. The dashboard is a read-only view of the same workspace, in a browser.",
      command: "cd apps/web-dashboard\nnpm install\nnpm run dev",
    },
    {
      title: "Or drive it from the command line",
      body: "The CLI links the engine directly — no server, no app.",
      command: "cargo run -p notewise-cli -- import ./meeting.wav\ncargo run -p notewise-cli -- export <meeting-id>",
    },
  ],
};

/** A first guess at the visitor's platform, so the right tab is already open. */
function detect(): Os {
  const hint = `${navigator.userAgent} ${navigator.platform ?? ""}`.toLowerCase();
  if (hint.includes("win")) return "windows";
  if (hint.includes("linux") || hint.includes("android")) return "linux";
  // macOS last as the default: it is the only platform where everything works, so it is the
  // least misleading fallback when the guess fails.
  return "mac";
}

function escapeHtml(value: string): string {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function renderPlatforms(current: Os): void {
  const host = document.querySelector<HTMLElement>("#platforms");
  if (!host) return;

  host.innerHTML = PLATFORMS.map((platform, index) => {
    const badge = BADGE[platform.status];
    const active = platform.os === current;
    return `
      <article class="card p-6 transition-all duration-300 ${
        active ? "border-accent/50 bg-accent/[0.04]" : ""
      }" data-reveal style="--reveal-delay:${index * 80}ms">
        <div class="flex items-center justify-between gap-3">
          <h2 class="text-[16px] font-medium">${platform.name}</h2>
          <span class="rounded-full px-2.5 py-1 text-[11px] font-semibold ${badge.classes}">${badge.label}</span>
        </div>
        <p class="mt-3 text-[13.5px] leading-relaxed text-ink-muted">${platform.summary}</p>
        ${active ? '<p class="mt-3 text-[12px] text-accent">Detected — instructions below</p>' : ""}
      </article>`;
  }).join("");
}

function renderSteps(os: Os): void {
  const host = document.querySelector<HTMLElement>("#steps");
  if (!host) return;

  host.innerHTML = STEPS[os]
    .map(
      (step, index) => `
      <div class="card overflow-hidden" data-reveal style="--reveal-delay:${index * 60}ms">
        <div class="flex items-start gap-4 p-6 ${step.command ? "pb-4" : ""}">
          <span class="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-full
                       border border-hairline font-mono text-[12px] text-ink-muted">${index + 1}</span>
          <div class="min-w-0">
            <h3 class="text-[15px] font-medium">${step.title}</h3>
            ${step.body ? `<p class="mt-1.5 text-[13.5px] leading-relaxed text-ink-muted">${step.body}</p>` : ""}
          </div>
        </div>
        ${
          step.command
            ? `<div class="relative border-t border-hairline bg-black/30">
                 <pre class="overflow-x-auto px-6 py-4 font-mono text-[12.5px] leading-relaxed text-ink-muted"><code>${escapeHtml(step.command)}</code></pre>
                 <button type="button"
                         class="copy absolute right-3 top-3 rounded-lg border border-hairline bg-surface px-2.5 py-1
                                text-[11.5px] text-ink-muted transition-colors hover:bg-white/5 hover:text-ink"
                         data-command="${escapeHtml(step.command)}">Copy</button>
               </div>`
            : ""
        }
      </div>`,
    )
    .join("");

  wireCopy();
  revealAll(host);
}

/**
 * Copy a command.
 *
 * `navigator.clipboard` needs a secure context, which `file://` and plain http are not — so
 * there is a fallback, and a button that cannot copy says so rather than doing nothing.
 */
function wireCopy(): void {
  for (const button of document.querySelectorAll<HTMLButtonElement>("button.copy")) {
    button.addEventListener("click", async () => {
      const command = button.dataset.command ?? "";
      let copied = false;

      try {
        await navigator.clipboard.writeText(command);
        copied = true;
      } catch {
        const scratch = document.createElement("textarea");
        scratch.value = command;
        scratch.setAttribute("readonly", "");
        scratch.style.position = "fixed";
        scratch.style.opacity = "0";
        document.body.append(scratch);
        scratch.select();
        try {
          copied = document.execCommand("copy");
        } catch {
          copied = false;
        }
        scratch.remove();
      }

      button.textContent = copied ? "Copied" : "Press ⌘C";
      button.classList.toggle("text-accent", copied);
      setTimeout(() => {
        button.textContent = "Copy";
        button.classList.remove("text-accent");
      }, 1600);
    });
  }
}

function revealAll(scope: ParentNode): void {
  const targets = scope.querySelectorAll<HTMLElement>("[data-reveal]");

  if (REDUCED || !("IntersectionObserver" in window)) {
    targets.forEach((element) => element.classList.add("is-visible"));
    return;
  }

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        entry.target.classList.add("is-visible");
        observer.unobserve(entry.target);
      }
    },
    { threshold: 0.1, rootMargin: "0px 0px -6% 0px" },
  );

  targets.forEach((element) => observer.observe(element));
}

function selectOs(os: Os): void {
  for (const tab of document.querySelectorAll<HTMLButtonElement>(".os-tab")) {
    tab.setAttribute("aria-selected", String(tab.dataset.os === os));
  }
  renderPlatforms(os);
  renderSteps(os);
  revealAll(document);
}

function setupChrome(): void {
  const nav = document.querySelector<HTMLElement>("#nav");
  const progress = document.querySelector<HTMLElement>("#progress");
  let queued = false;

  const update = () => {
    const y = window.scrollY;
    if (nav) {
      const solid = y > 24;
      nav.classList.toggle("border-hairline", solid);
      nav.classList.toggle("bg-bg/80", solid);
      nav.classList.toggle("backdrop-blur-xl", solid);
    }
    if (progress) {
      const total = document.documentElement.scrollHeight - window.innerHeight;
      progress.style.width = `${total > 0 ? Math.min(100, (y / total) * 100) : 0}%`;
    }

    if (!REDUCED) {
      for (const layer of document.querySelectorAll<HTMLElement>("[data-parallax]")) {
        const rate = Number.parseFloat(layer.dataset.parallax ?? "0");
        const box = layer.getBoundingClientRect();
        const offset = (box.top + box.height / 2 - window.innerHeight / 2) * -rate;
        layer.style.transform = `translate3d(0, ${offset.toFixed(1)}px, 0)`;
      }
    }
  };

  const onScroll = () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      update();
    });
  };

  update();
  window.addEventListener("scroll", onScroll, { passive: true });
  window.addEventListener("resize", update);
}

function init(): void {
  for (const tab of document.querySelectorAll<HTMLButtonElement>(".os-tab")) {
    tab.addEventListener("click", () => selectOs(tab.dataset.os as Os));
  }

  selectOs(detect());
  setupChrome();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
