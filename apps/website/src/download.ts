/**
 * The install page: the download cards, the macOS first-launch walkthrough, and building from
 * source on every platform.
 *
 * The Mac card is the same one the overview shows, resolved against the real GitHub release
 * (see `downloads.ts`). Windows and Ubuntu say "coming soon" there, and this page's source-build
 * steps are how to run Notewise on them today.
 */

import "./style.css";
import { detectOs, mountDownloads, type Os } from "./downloads";

const REDUCED = !document.documentElement.classList.contains("motion");

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
      "Apple silicon or Intel. The desktop app, recording, GPU-accelerated transcription: everything except system audio.",
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
      body: "The record and whisper-metal features pull the platform audio SDK and a cmake build of whisper.cpp with Metal (the GPU-accelerated transcription), so this first build is slow.",
      command: "cargo run -p notewise-cli --features record,whisper-metal -- serve",
    },
    {
      title: "Open the app's interface",
      body: "In a second terminal. The desktop app's interface, in your browser at http://localhost:1420, against the engine you just started.",
      command: "cd apps/desktop\nnpm install\nnpm run dev",
    },
  ],
  windows: [
    ...common(
      "# In PowerShell: Rust, cmake, Node, and the MSVC build tools\nwinget install Rustlang.Rustup\nwinget install Kitware.CMake\nwinget install OpenJS.NodeJS\nwinget install Microsoft.VisualStudio.2022.BuildTools",
    ),
    {
      title: "Run the engine",
      body: "Microphone capture is untested on Windows. This default build searches, answers questions and runs the agent over what is in the workspace; it cannot transcribe. Importing audio needs the record and whisper features: cargo run -p notewise-cli --features record,whisper -- serve.",
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
      body: "Microphone capture through ALSA is untested. This default build searches, answers questions and runs the agent over what is in the workspace; it cannot transcribe. Importing audio needs the record and whisper features, and record needs the ALSA headers: sudo apt install -y libasound2-dev, then cargo run -p notewise-cli --features record,whisper -- serve.",
      command: "cargo run -p notewise-cli -- serve",
    },
    {
      title: "Use the web dashboard",
      body: "There is no Linux desktop shell yet. The dashboard is a read-only view of the same workspace, in a browser.",
      command: "cd apps/web-dashboard\nnpm install\nnpm run dev",
    },
    {
      title: "Or drive it from the command line",
      body: "The CLI links the engine directly: no server, no app. Its import command transcribes a 32-bit float WAV, and exists only in a build with the whisper feature.",
      command: "cargo run -p notewise-cli --features whisper -- import ./meeting.wav\ncargo run -p notewise-cli -- export <meeting-id>",
    },
  ],
};

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
        ${active ? '<p class="mt-3 text-[12px] text-accent">Detected. Instructions below</p>' : ""}
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
 * `navigator.clipboard` needs a secure context, which `file://` and plain http are not, so
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
  const downloads = document.querySelector<HTMLElement>("#downloads");
  if (downloads) {
    mountDownloads(downloads, { sourceHref: "#source", firstLaunchHref: "#first-launch" });
  }

  for (const tab of document.querySelectorAll<HTMLButtonElement>(".os-tab")) {
    tab.addEventListener("click", () => selectOs(tab.dataset.os as Os));
  }

  selectOs(detectOs());
  setupChrome();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
