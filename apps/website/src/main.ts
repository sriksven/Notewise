/**
 * The site's behaviour: scroll reveals, the sticky pipeline, parallax, counters, and the
 * content lists that are generated rather than hand-written twice.
 *
 * # No framework
 *
 * This is a static document with scroll effects. React would ship ~140 kB to render markup
 * that never changes, and every effect here is DOM-level anyway — an IntersectionObserver and
 * a scroll listener. The whole bundle is a few kilobytes.
 *
 * # Motion is optional, everywhere
 *
 * `prefers-reduced-motion` is checked once and every animated path is skipped when it is set.
 * The CSS handles the reveals; this file handles the parts CSS cannot, which is why the check
 * appears here too rather than only in the stylesheet.
 */

import "./style.css";

const REDUCED = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

// ---------------------------------------------------------------- content

/**
 * What the app does. One list, rendered into the grid.
 *
 * Kept in the code rather than the markup because every entry has the same shape, and a
 * fourteen-item grid written by hand is fourteen chances for one card to drift out of line.
 */
const FEATURES: Array<{ icon: string; title: string; body: string }> = [
  {
    icon: "◉",
    title: "Record or import",
    body: "Capture from your microphone, or drop in audio you already have. WAV, MP3, M4A, FLAC and more.",
  },
  {
    icon: "≋",
    title: "Local transcription",
    body: "Whisper on your own hardware, GPU-accelerated on Apple silicon. large-v3-turbo is the one worth downloading.",
  },
  {
    icon: "◑",
    title: "Speaker separation",
    body: "The transcript reads as a conversation. Names come from the meeting app when the browser extension is running.",
  },
  {
    icon: "✦",
    title: "Summaries that link back",
    body: "Decisions and action items are first-class objects with owners and dates — not a paragraph you have to re-read.",
  },
  {
    icon: "⌕",
    title: "Search by word and by meaning",
    body: "Full-text over everything, plus optional local embeddings so “pricing” finds the meeting that said “cost structure”.",
  },
  {
    icon: "❝",
    title: "Ask anything, with citations",
    body: "Question one meeting, one note, or the whole workspace. Answers cite their sources and admit when the answer is not there.",
  },
  {
    icon: "⬡",
    title: "An agent that does the reading",
    body: "Give it a task; it searches, reads and writes up what it found. It can create a note and nothing else.",
  },
  {
    icon: "▤",
    title: "A real block editor",
    body: "Headings, lists, to-dos, code and quotes, with Markdown shortcuts. Saved as Markdown you can read without this app.",
  },
  {
    icon: "⇄",
    title: "Notes attached to meetings",
    body: "Your own account of what happened, beside the machine's. Notes outlive the meeting and can reference several.",
  },
  {
    icon: "⌦",
    title: "Delete that you can undo",
    body: "Notes and meetings go to a trash and stay there until you empty it. Nothing expires on a timer.",
  },
  {
    icon: "⌁",
    title: "Connectors",
    body: "Write every meeting into a Markdown vault for Obsidian, or POST it to a webhook you control, signed.",
  },
  {
    icon: "⌘",
    title: "Agent access over MCP",
    body: "Point Claude or any MCP client at your workspace. Read-only unless you explicitly allow writes.",
  },
];

/**
 * What does not work yet.
 *
 * On the page for the same reason it is in the app's About screen: someone who discovers that
 * system audio does not work by recording a call and getting half a transcript is worse off
 * than someone who was told here.
 */
const LIMITATIONS: Array<{ title: string; body: string }> = [
  {
    title: "System audio is not captured",
    body: "The microphone hears you and the room, not the other people on a call. macOS only grants screen-audio permission to a signed, bundled app, and this one is not signed yet.",
  },
  {
    title: "macOS only, in practice",
    body: "The engine and CLI build anywhere Rust does. The desktop shell is macOS; Windows and Linux builds are not done.",
  },
  {
    title: "There are no installers",
    body: "No release channel, no signed binaries, no auto-update. You build it from source — the install page has the commands.",
  },
  {
    title: "Speakers are separated, not identified",
    body: "Diarization splits on pauses rather than voices, so you get Speaker 1 and Speaker 2 unless the browser extension supplies real names.",
  },
  {
    title: "Cloud sync, mobile and teams are not built",
    body: "Those directories exist and are empty on purpose. Shipping half of four product categories at once is the failure the roadmap's phase gating prevents.",
  },
];

function renderLists(): void {
  const grid = document.querySelector<HTMLElement>("#feature-grid");
  if (grid) {
    grid.innerHTML = FEATURES.map(
      (feature, index) => `
        <article class="card group p-6 transition-all duration-300 hover:-translate-y-1 hover:border-ink-faint"
                 data-reveal style="--reveal-delay:${(index % 3) * 90}ms">
          <span class="text-[20px] text-accent transition-transform duration-300 group-hover:scale-110 inline-block">${feature.icon}</span>
          <h3 class="mt-3 text-[15px] font-medium">${feature.title}</h3>
          <p class="mt-2 text-[13.5px] leading-relaxed text-ink-muted">${feature.body}</p>
        </article>`,
    ).join("");
  }

  const limits = document.querySelector<HTMLElement>("#limitations");
  if (limits) {
    limits.innerHTML = LIMITATIONS.map(
      (limit, index) => `
        <div class="card flex items-start gap-4 p-5" data-reveal style="--reveal-delay:${index * 70}ms">
          <span class="mt-1 shrink-0 text-ink-faint">○</span>
          <div>
            <h3 class="text-[15px] font-medium">${limit.title}</h3>
            <p class="mt-1 text-[13.5px] leading-relaxed text-ink-muted">${limit.body}</p>
          </div>
        </div>`,
    ).join("");
  }
}

// ---------------------------------------------------------------- reveal

/**
 * Fade sections in as they arrive.
 *
 * Unobserved once shown: a reveal that re-runs on every scroll past makes a long page flicker,
 * and the observer is doing no work after the first pass.
 */
function setupReveal(): void {
  const targets = document.querySelectorAll<HTMLElement>("[data-reveal]");

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
    // Triggered slightly before the element is fully on screen, so the motion finishes as it
    // settles rather than starting once it has already arrived.
    { threshold: 0.12, rootMargin: "0px 0px -8% 0px" },
  );

  targets.forEach((element) => observer.observe(element));
}

// ---------------------------------------------------------------- pipeline

/**
 * Swap the sticky panel as each step scrolls through the middle of the viewport.
 *
 * Driven by which stage is nearest the centre rather than by "last one that entered": with
 * fast scrolling, entry events arrive out of order and the panel ends up on the wrong step.
 */
function setupPipeline(): void {
  const stages = [...document.querySelectorAll<HTMLElement>("[data-stage]")];
  const panels = [...document.querySelectorAll<HTMLElement>("[data-panel]")];
  if (stages.length === 0 || panels.length === 0) return;

  const show = (id: string) => {
    for (const panel of panels) {
      panel.classList.toggle("is-active", panel.dataset.panel === id);
    }
  };

  show("1");

  const update = () => {
    const middle = window.innerHeight / 2;
    let nearest = stages[0];
    let best = Infinity;

    for (const stage of stages) {
      const box = stage.getBoundingClientRect();
      const distance = Math.abs(box.top + box.height / 2 - middle);
      if (distance < best) {
        best = distance;
        nearest = stage;
      }
    }

    if (nearest.dataset.stage) show(nearest.dataset.stage);
  };

  update();
  window.addEventListener("scroll", onScroll(update), { passive: true });
  window.addEventListener("resize", update);
}

// ---------------------------------------------------------------- chrome

/** Solidify the header once the page has moved, so it reads over content. */
function setupNav(): void {
  const nav = document.querySelector<HTMLElement>("#nav");
  const progress = document.querySelector<HTMLElement>("#progress");

  const update = () => {
    const y = window.scrollY;

    if (nav) {
      const solid = y > 24;
      nav.classList.toggle("border-hairline", solid);
      nav.classList.toggle("bg-bg/80", solid);
      nav.classList.toggle("backdrop-blur-xl", solid);
    }

    if (progress) {
      // `scrollHeight - innerHeight` is the distance actually scrollable; using scrollHeight
      // alone leaves the bar short of the end by exactly one viewport.
      const total = document.documentElement.scrollHeight - window.innerHeight;
      progress.style.width = `${total > 0 ? Math.min(100, (y / total) * 100) : 0}%`;
    }
  };

  update();
  window.addEventListener("scroll", onScroll(update), { passive: true });
  window.addEventListener("resize", update);
}

/** Drift decorative layers against the scroll. */
function setupParallax(): void {
  if (REDUCED) return;
  const layers = [...document.querySelectorAll<HTMLElement>("[data-parallax]")];
  if (layers.length === 0) return;

  const update = () => {
    for (const layer of layers) {
      const rate = Number.parseFloat(layer.dataset.parallax ?? "0");
      const box = layer.getBoundingClientRect();
      // Relative to the element's own position, so a layer far down the page does not start
      // its travel already thousands of pixels out.
      const offset = (box.top + box.height / 2 - window.innerHeight / 2) * -rate;
      layer.style.transform = `translate3d(0, ${offset.toFixed(1)}px, 0)`;
    }
  };

  update();
  window.addEventListener("scroll", onScroll(update), { passive: true });
  window.addEventListener("resize", update);
}

/** Count the hero's numbers up when they first appear. */
function setupCounters(): void {
  const counters = [...document.querySelectorAll<HTMLElement>("[data-count]")];
  if (counters.length === 0) return;

  const settle = (element: HTMLElement) => {
    element.textContent = Number(element.dataset.count ?? "0").toLocaleString();
  };

  if (REDUCED || !("IntersectionObserver" in window)) {
    counters.forEach(settle);
    return;
  }

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const element = entry.target as HTMLElement;
        observer.unobserve(element);

        const target = Number(element.dataset.count ?? "0");
        if (target === 0) {
          settle(element);
          continue;
        }

        const started = performance.now();
        const step = (now: number) => {
          const progress = Math.min(1, (now - started) / 1400);
          // Ease out, so it decelerates into the final number rather than stopping dead.
          const eased = 1 - Math.pow(1 - progress, 3);
          element.textContent = Math.round(target * eased).toLocaleString();
          if (progress < 1) requestAnimationFrame(step);
        };
        requestAnimationFrame(step);
      }
    },
    { threshold: 0.6 },
  );

  counters.forEach((element) => observer.observe(element));
}

/**
 * Coalesce scroll work into one frame.
 *
 * A scroll listener can fire far more often than the display refreshes, and doing layout reads
 * in each one is how a page with four scroll effects starts to stutter.
 */
function onScroll(run: () => void): () => void {
  let queued = false;
  return () => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      run();
    });
  };
}

// ---------------------------------------------------------------- start

function init(): void {
  renderLists();
  // After rendering: the feature cards carry `data-reveal` and do not exist until then.
  setupReveal();
  setupPipeline();
  setupNav();
  setupParallax();
  setupCounters();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
