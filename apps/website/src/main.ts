/**
 * The overview page: pinned scroll scenes, the marquee, and the download cards.
 *
 * # No framework
 *
 * This is a static document with scroll effects. React would ship ~140 kB to render markup that
 * never changes, and every effect here is DOM-level anyway. Each pinned scene reduces to one
 * number (how far through it the reader is, from 0 to 1) and draws itself from that. One
 * scroll listener, coalesced to one frame, feeds all of them.
 *
 * # Motion is optional, everywhere
 *
 * An inline script in the page adds `.motion` to `<html>` before first paint unless the visitor
 * has asked for reduced motion. Every pinned or scrubbed effect is gated on that class in the CSS,
 * and on `MOTION` here: without it the scenes are ordinary sections in their finished state.
 */

import "./style.css";
import { mountDownloads } from "./downloads";
import { cardHover, decodeLabels, downloadBurst, liveDemo, particles } from "./effects";

const MOTION = document.documentElement.classList.contains("motion");
const FINE_POINTER = window.matchMedia("(hover: hover) and (pointer: fine)").matches;

const clamp = (value: number, min = 0, max = 1) => Math.min(max, Math.max(min, value));
const easeOut = (t: number) => 1 - Math.pow(1 - t, 3);

// ---------------------------------------------------------------- content

/**
 * What the app does. One list, rendered into the grid.
 *
 * Kept in the code rather than the markup because every entry has the same shape, and a
 * fourteen-item list written by hand is fourteen chances for one card to drift out of line.
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
    body: "The transcript reads as a conversation. Name a voice once and it applies throughout, or merge two the diarizer split apart.",
  },
  {
    icon: "✦",
    title: "Summaries that link back",
    body: "Decisions and action items are first-class objects with owners and dates, not a paragraph you have to re-read.",
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
    icon: "◇",
    title: "Questions while people can still answer",
    body: "It watches a live meeting and flags what was left vague: a decision with no owner, a date nobody actually said.",
  },
  {
    icon: "◫",
    title: "Tickets without a tracker",
    body: "Action items become work you can assign, date and close in the app. No Jira, no second tool to keep in sync, no account.",
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
    title: "System audio is not verified yet",
    body: "The microphone hears you and the room. The other people on a call come through macOS system audio, which needs the Screen & System Audio Recording permission switched on by hand and a relaunch, and has not yet been run end to end in a release build.",
  },
  {
    title: "Signed by the project, not by Apple",
    body: "The Mac app is signed with Notewise's own certificate rather than an Apple Developer ID, so macOS blocks the first launch until you choose Open Anyway in Privacy & Security. Once per download. There is no auto-update yet.",
  },
  {
    title: "macOS only, for now",
    body: "The downloadable app is for Apple silicon Macs. Windows and Ubuntu builds are coming; until then the engine, CLI and web dashboard build from source anywhere Rust does.",
  },
  {
    title: "Speakers are separated, not recognised",
    body: "Voice-based separation exists but is off by default and needs a model download; without it, splitting is on pauses. Either way you get Speaker 1 and Speaker 2 until you name them, or the browser extension does.",
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
        <article class="feature" data-anim="${index % 4}">
          <span class="feature-num">${String(index + 1).padStart(2, "0")}</span>
          <span class="feature-icon">${feature.icon}</span>
          <h3 class="mt-5 text-[18px] font-semibold tracking-[-0.01em]">${feature.title}</h3>
          <p class="mt-2 text-[13.5px] leading-relaxed text-ink-muted">${feature.body}</p>
        </article>`,
    ).join("");
  }

  const limits = document.querySelector<HTMLElement>("#limitations");
  if (limits) {
    limits.innerHTML = LIMITATIONS.map(
      (limit, index) => `
        <div class="card flex items-start gap-4 p-5" data-reveal="${index % 2 ? "right" : "left"}" style="--reveal-delay:${index * 70}ms">
          <span class="mt-1 shrink-0 text-ink-faint">○</span>
          <div>
            <h3 class="text-[15px] font-medium">${limit.title}</h3>
            <p class="mt-1 text-[13.5px] leading-relaxed text-ink-muted">${limit.body}</p>
          </div>
        </div>`,
    ).join("");
  }
}

// ---------------------------------------------------------------- text splitting

/** Interpolated across the second headline line, standing in for a gradient. */
const GRADIENT = ["#a5b0fa", "#ededf0", "#7c8cf8"];

function mix(from: string, to: string, t: number): string {
  const a = Number.parseInt(from.slice(1), 16);
  const b = Number.parseInt(to.slice(1), 16);
  const channel = (shift: number) =>
    Math.round(((a >> shift) & 255) + (((b >> shift) & 255) - ((a >> shift) & 255)) * t);
  return `rgb(${channel(16)}, ${channel(8)}, ${channel(0)})`;
}

function gradientAt(t: number): string {
  const scaled = t * (GRADIENT.length - 1);
  const index = Math.min(GRADIENT.length - 2, Math.floor(scaled));
  return mix(GRADIENT[index], GRADIENT[index + 1], scaled - index);
}

/**
 * Split the headline into characters so each can rise on its own.
 *
 * A clipped wrapper per word, not per character: per character, the clip would cut the
 * neighbouring glyph's overhang. The whole line keeps its text for screen readers via the
 * heading's label, and the fragments are hidden from them.
 */
function splitHeadline(): void {
  const lines = [...document.querySelectorAll<HTMLElement>("[data-split]")];
  const heading = lines[0]?.closest("h1");
  if (heading) heading.setAttribute("aria-label", lines.map((line) => line.textContent?.trim()).join(" "));

  let order = 0;
  for (const line of lines) {
    const text = line.textContent?.trim() ?? "";
    const gradient = line.dataset.split === "gradient";
    const letters = text.replace(/\s/g, "").length;
    let seen = 0;

    line.innerHTML = text
      .split(" ")
      .map((word) => {
        const chars = [...word]
          .map((char) => {
            const color = gradient ? `color:${gradientAt(letters > 1 ? seen / (letters - 1) : 0)};` : "";
            seen += 1;
            return `<span class="split-char" style="--i:${order++};${color}">${char}</span>`;
          })
          .join("");
        return `<span class="split-word" aria-hidden="true">${chars}</span>`;
      })
      .join(" ");
    line.classList.add("is-split");
  }
}

// ---------------------------------------------------------------- scenes

interface Scene {
  element: HTMLElement;
  /** Called with how far through the scene the reader is, 0 → 1. */
  update(progress: number): void;
  /** Called on resize, before `update`, for anything that depends on layout. */
  measure?(): void;
}

/**
 * Progress through a pinned scene: 0 when its top reaches the top of the viewport, 1 when its
 * bottom reaches the bottom: the stretch during which its sticky frame is held in place.
 */
function progressOf(element: HTMLElement): number {
  const box = element.getBoundingClientRect();
  const travel = box.height - window.innerHeight;
  if (travel <= 0) return box.top <= 0 ? 1 : 0;
  return clamp(-box.top / travel);
}

function heroScene(): Scene | null {
  const element = document.querySelector<HTMLElement>('[data-scene="hero"]');
  const copy = document.querySelector<HTMLElement>("[data-hero-copy]");
  const device = document.querySelector<HTMLElement>("[data-hero-device]");
  const cue = document.querySelector<HTMLElement>(".scroll-cue");
  if (!element || !copy || !device) return null;

  // How far the window must rise to sit centred, measured from its resting place in the layout.
  let rise = 0;

  return {
    element,
    measure() {
      const height = device.offsetHeight;
      const target = Math.max(84, (window.innerHeight - height) / 2);
      rise = Math.max(0, device.offsetTop - target);
    },
    update(progress) {
      const settle = easeOut(clamp(progress / 0.72));
      const lift = clamp(progress * 1.9);

      copy.style.transform = `translate3d(0, ${(-progress * window.innerHeight * 0.24).toFixed(1)}px, 0) scale(${(1 - progress * 0.08).toFixed(3)})`;
      copy.style.opacity = (1 - lift).toFixed(3);
      copy.style.filter = `blur(${(lift * 10).toFixed(1)}px)`;
      if (cue) cue.style.opacity = clamp(1 - progress * 6).toFixed(3);

      device.style.transform =
        `perspective(1400px) translate3d(0, ${(-rise * settle).toFixed(1)}px, 0) ` +
        `rotateX(${(32 * (1 - settle)).toFixed(2)}deg) scale(${(0.88 + 0.12 * settle).toFixed(3)})`;
    },
  };
}

function pipelineScene(): Scene | null {
  const element = document.querySelector<HTMLElement>('[data-scene="pipe"]');
  if (!element) return null;

  const layers = [...element.querySelectorAll<HTMLElement>("[data-layer]")];
  const steps = [...element.querySelectorAll<HTMLElement>("[data-step]")];
  const count = layers.length;

  const eq = element.querySelector<HTMLElement>("[data-eq]");
  if (eq) {
    // Fixed heights rather than random ones: the page should look the same on every visit.
    eq.innerHTML = Array.from({ length: 44 }, (_, index) => {
      const height = 0.25 + 0.75 * Math.abs(Math.sin(index * 0.55) * Math.cos(index * 0.17));
      return `<span style="--h:${height.toFixed(2)};--delay:${(-(index % 7) * 0.13).toFixed(2)}s"></span>`;
    }).join("");
  }

  let active = -1;

  return {
    element,
    update(progress) {
      const position = progress * count;
      element.style.setProperty("--p", progress.toFixed(4));

      layers.forEach((layer, index) => {
        const local = clamp(position - index);
        const enter = index === 0 ? 1 : clamp((position - index) / 0.16);
        const leave = index === count - 1 ? 1 : clamp((index + 1 - position) / 0.14);
        const shown = Math.min(enter, leave);

        layer.style.setProperty("--t", local.toFixed(3));
        layer.style.opacity = shown.toFixed(3);
        layer.style.transform = `translate3d(0, ${((1 - enter) * 40 - (1 - leave) * 40).toFixed(1)}px, 0) scale(${(0.96 + 0.04 * shown).toFixed(3)})`;
        layer.style.visibility = shown <= 0.001 ? "hidden" : "visible";
      });

      const now = Math.min(count - 1, Math.floor(position));
      if (now !== active) {
        active = now;
        steps.forEach((step, index) => step.classList.toggle("is-active", index === now));
      }
    },
  };
}

function graphScene(): Scene | null {
  const element = document.querySelector<HTMLElement>('[data-scene="graph"]');
  if (!element) return null;

  const edges = [...element.querySelectorAll<SVGPathElement>("[data-edge]")];
  const pulses = [...element.querySelectorAll<SVGCircleElement>("[data-pulse]")];
  const nodes = [...element.querySelectorAll<SVGGElement>("[data-node]")];
  const spokes = nodes.length;

  return {
    element,
    update(progress) {
      const run = clamp(progress / 0.78);
      edges.forEach((edge, index) => {
        if (index < spokes) {
          const drawn = clamp((run - index * 0.1) / 0.42);
          edge.style.setProperty("--draw", (1 - drawn).toFixed(3));
          nodes[index]?.style.setProperty("--pop", easeOut(clamp((drawn - 0.65) / 0.35)).toFixed(3));
          // Data only flows along an edge once it exists.
          pulses[index]?.style.setProperty("--flow", clamp((drawn - 0.9) / 0.1).toFixed(3));
        } else {
          // The second-degree link, decision to ticket, arrives last.
          edge.style.setProperty("--show", clamp((run - 0.86) / 0.14).toFixed(3));
        }
      });
    },
  };
}

function setupScenes(): void {
  if (!MOTION) {
    // The pipeline has to show a step as current even without motion; all of them are.
    document.querySelectorAll(".step").forEach((step) => step.classList.add("is-active"));
    return;
  }

  const scenes = [heroScene(), pipelineScene(), graphScene()].filter(
    (scene): scene is Scene => scene !== null,
  );

  const draw = () => {
    for (const scene of scenes) {
      const box = scene.element.getBoundingClientRect();
      // Off-screen scenes are skipped, but still drawn once at their nearest end so a fast jump
      // past one never leaves it half-finished when scrolled back to.
      if (box.bottom < -50 || box.top > window.innerHeight + 50) {
        scene.update(box.bottom < 0 ? 1 : 0);
        continue;
      }
      scene.update(progressOf(scene.element));
    }
  };

  const remeasure = () => {
    for (const scene of scenes) scene.measure?.();
    draw();
  };

  remeasure();
  window.addEventListener("scroll", onFrame(draw), { passive: true });
  window.addEventListener("resize", onFrame(remeasure));
  // Web fonts and late layout shift the measurements; one more pass once everything is in.
  window.addEventListener("load", remeasure);
}

// ---------------------------------------------------------------- marquee

/**
 * Two rows of type that drift on their own and surge with scrolling, leaning into the direction
 * of travel. Runs only while the band is on screen.
 */
function setupMarquee(): void {
  const rows = [...document.querySelectorAll<HTMLElement>("[data-marquee]")];
  if (rows.length === 0) return;

  const tracks = rows.map((row) => {
    const inner = row.querySelector<HTMLElement>(".marquee-row");
    if (!inner) return null;
    // Two copies, so the seam is never on screen.
    inner.innerHTML = `<span>${inner.textContent}&nbsp;</span><span>${inner.textContent}&nbsp;</span>`;
    return { inner, direction: Number(row.dataset.marquee ?? "1"), x: 0, width: 0 };
  });

  if (!MOTION) return;

  let velocity = 0;
  let lastY = window.scrollY;
  let lastTime = performance.now();
  let running = false;

  const frame = (now: number) => {
    if (!running) return;
    const dt = Math.min(0.05, (now - lastTime) / 1000);
    lastTime = now;

    const y = window.scrollY;
    const delta = y - lastY;
    lastY = y;
    // Smoothed, so a single big wheel tick reads as a lean rather than a jolt.
    velocity += (delta - velocity) * 0.12;

    for (const track of tracks) {
      if (!track) continue;
      if (track.width === 0) track.width = track.inner.scrollWidth / 2;
      const speed = (60 + Math.abs(velocity) * 28) * dt;
      track.x -= speed * track.direction;
      if (track.x <= -track.width) track.x += track.width;
      if (track.x > 0) track.x -= track.width;
      const lean = clamp(velocity * -0.35, -14, 14);
      track.inner.style.transform = `translate3d(${track.x.toFixed(1)}px, 0, 0) skewX(${lean.toFixed(2)}deg)`;
    }

    requestAnimationFrame(frame);
  };

  const band = rows[0].parentElement;
  if (!band || !("IntersectionObserver" in window)) return;

  new IntersectionObserver((entries) => {
    const visible = entries.some((entry) => entry.isIntersecting);
    if (visible && !running) {
      running = true;
      lastTime = performance.now();
      lastY = window.scrollY;
      requestAnimationFrame(frame);
    } else if (!visible) {
      running = false;
    }
  }).observe(band);

  window.addEventListener("resize", () => {
    for (const track of tracks) if (track) track.width = 0;
  });
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

  if (!MOTION || !("IntersectionObserver" in window)) {
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

// ---------------------------------------------------------------- chrome

/** Solidify the header once the page has moved, and run the progress bar. */
function setupNav(): void {
  const nav = document.querySelector<HTMLElement>("#nav");
  const progress = document.querySelector<HTMLElement>("#progress");
  const logo = document.querySelector<HTMLElement>(".logo-mark");

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

    if (logo && MOTION) logo.style.setProperty("--scroll-turn", (y * 0.12).toFixed(1));
  };

  update();
  window.addEventListener("scroll", onFrame(update), { passive: true });
  window.addEventListener("resize", update);
}

/**
 * Scroll-linked touches outside the pinned scenes: parallax layers, the privacy headline that
 * un-zooms into place, and the outlined word drifting behind the finale.
 */
function setupDrift(): void {
  if (!MOTION) return;
  const layers = [...document.querySelectorAll<HTMLElement>("[data-parallax]")];
  const zooms = [...document.querySelectorAll<HTMLElement>("[data-zoom]")];
  const finale = document.querySelector<HTMLElement>('[data-scene="finale"]');
  const word = finale?.querySelector<HTMLElement>(".finale-word");

  const update = () => {
    const height = window.innerHeight;

    for (const layer of layers) {
      const rate = Number.parseFloat(layer.dataset.parallax ?? "0");
      const box = layer.getBoundingClientRect();
      // Relative to the element's own position, so a layer far down the page does not start its
      // travel already thousands of pixels out.
      const offset = (box.top + box.height / 2 - height / 2) * -rate;
      layer.style.transform = `translate3d(0, ${offset.toFixed(1)}px, 0)`;
    }

    for (const zoom of zooms) {
      const box = zoom.getBoundingClientRect();
      const arrived = clamp(1 - (box.top - height * 0.3) / (height * 0.6));
      zoom.style.setProperty("--z", (0.78 + 0.22 * easeOut(arrived)).toFixed(3));
    }

    if (finale && word) {
      const box = finale.getBoundingClientRect();
      const through = clamp((height - box.top) / (height + box.height));
      word.style.setProperty("--fx", `${((0.5 - through) * 260).toFixed(1)}px`);
    }
  };

  update();
  window.addEventListener("scroll", onFrame(update), { passive: true });
  window.addEventListener("resize", update);
}

/** Count the stats up when they first appear. */
function setupCounters(): void {
  const counters = [...document.querySelectorAll<HTMLElement>("[data-count]")];
  if (counters.length === 0) return;

  const settle = (element: HTMLElement) => {
    element.textContent = Number(element.dataset.count ?? "0").toLocaleString();
  };

  if (!MOTION || !("IntersectionObserver" in window)) {
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
          const progress = Math.min(1, (now - started) / 1600);
          element.textContent = Math.round(target * easeOut(progress)).toLocaleString();
          if (progress < 1) requestAnimationFrame(step);
        };
        requestAnimationFrame(step);
      }
    },
    { threshold: 0.6 },
  );

  counters.forEach((element) => observer.observe(element));
}

// ---------------------------------------------------------------- pointer

/**
 * The pointer's effects: a soft spotlight that follows it, cards that tilt toward it, and
 * buttons that lean after it. Mouse and trackpad only; on touch there is no hover to follow.
 */
function setupPointer(): void {
  if (!MOTION || !FINE_POINTER) return;

  const spotlight = document.querySelector<HTMLElement>(".spotlight");
  if (spotlight) {
    window.addEventListener(
      "pointermove",
      onFrameWith((event: PointerEvent) => {
        spotlight.style.setProperty("--mx", `${event.clientX}px`);
        spotlight.style.setProperty("--my", `${event.clientY}px`);
        spotlight.classList.add("is-on");
      }),
      { passive: true },
    );
  }

  for (const card of document.querySelectorAll<HTMLElement>("[data-tilt]")) {
    card.addEventListener("pointermove", (event) => {
      // Not until the reveal has finished: both want the same transform.
      if (!card.classList.contains("is-visible")) return;
      const box = card.getBoundingClientRect();
      const x = (event.clientX - box.left) / box.width;
      const y = (event.clientY - box.top) / box.height;
      card.classList.add("is-tilting");
      card.style.transform = `perspective(900px) rotateX(${((0.5 - y) * 9).toFixed(2)}deg) rotateY(${((x - 0.5) * 11).toFixed(2)}deg) translateZ(0)`;
      card.style.setProperty("--sx", `${(x * 100).toFixed(1)}%`);
      card.style.setProperty("--sy", `${(y * 100).toFixed(1)}%`);
    });
    card.addEventListener("pointerleave", () => {
      card.classList.remove("is-tilting");
      card.style.transform = "";
    });
  }

  for (const button of document.querySelectorAll<HTMLElement>("[data-magnetic]")) {
    button.style.transition = "transform 400ms cubic-bezier(0.16, 1, 0.3, 1)";
    button.addEventListener("pointermove", (event) => {
      const box = button.getBoundingClientRect();
      const dx = event.clientX - (box.left + box.width / 2);
      const dy = event.clientY - (box.top + box.height / 2);
      button.style.transform = `translate3d(${(dx * 0.22).toFixed(1)}px, ${(dy * 0.32).toFixed(1)}px, 0)`;
    });
    button.addEventListener("pointerleave", () => {
      button.style.transform = "";
    });
  }
}

// ---------------------------------------------------------------- frame coalescing

/**
 * Coalesce scroll work into one frame.
 *
 * A scroll listener can fire far more often than the display refreshes, and doing layout reads
 * in each one is how a page with several scroll effects starts to stutter.
 */
function onFrame(run: () => void): () => void {
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

/** `onFrame` for handlers that need the latest event. */
function onFrameWith<E>(run: (event: E) => void): (event: E) => void {
  let latest: E | null = null;
  return (event: E) => {
    const queued = latest !== null;
    latest = event;
    if (queued) return;
    requestAnimationFrame(() => {
      if (latest !== null) run(latest);
      latest = null;
    });
  };
}

// ---------------------------------------------------------------- start

function init(): void {
  splitHeadline();
  renderLists();

  const downloads = document.querySelector<HTMLElement>("#downloads");
  if (downloads) {
    mountDownloads(downloads, {
      sourceHref: "./download/#source",
      firstLaunchHref: "./download/#first-launch",
    });
  }

  // After rendering: the cards carry `data-reveal` and do not exist until then.
  setupReveal();
  setupScenes();
  setupMarquee();
  setupNav();
  setupDrift();
  setupCounters();
  setupPointer();
  liveDemo();
  particles();
  decodeLabels();
  cardHover();
  downloadBurst();
}

if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", init);
} else {
  init();
}
