/**
 * Animations that are not driven by scroll: a live demo that plays on a timer, ambient particles,
 * a decode effect on section labels, hover effects on cards, and a burst when the download button
 * is pressed.
 *
 * Everything here checks `.motion` first, the same gate the scroll scenes use. With reduced
 * motion the content is all there and none of it moves.
 */

const MOTION = document.documentElement.classList.contains("motion");
const FINE_POINTER = window.matchMedia("(hover: hover) and (pointer: fine)").matches;

/** Runs `start` while `element` is on screen and `stop` once it leaves. */
function whileVisible(element: Element, start: () => void, stop: () => void): void {
  if (!("IntersectionObserver" in window)) {
    start();
    return;
  }
  new IntersectionObserver((entries) => {
    for (const entry of entries) (entry.isIntersecting ? start : stop)();
  }).observe(element);
}

// ---------------------------------------------------------------- live demo

/**
 * The hero's app window, recording: the timer ticks, each transcript line types itself in, and
 * what was extracted from it appears beside it. Then it clears and plays again.
 *
 * Timer-driven rather than scroll-driven, and only while the window is on screen.
 */
export function liveDemo(): void {
  const device = document.querySelector<HTMLElement>("[data-hero-device]");
  if (!MOTION || !device) return;

  const timer = device.querySelector<HTMLElement>("[data-live-timer]");
  const lines = [...device.querySelectorAll<HTMLElement>("[data-live-line]")];
  const items = [...device.querySelectorAll<HTMLElement>("[data-live-item]")];
  const texts = lines.map((line) => line.querySelector<HTMLElement>("[data-live-text]"));
  const full = texts.map((text) => text?.textContent ?? "");

  // Which extracted item each line produces; the last one comes after everything has been said.
  const produces: Array<number | null> = [0, null, 1];

  let seconds = 12 * 60 + 4;
  let clock = 0;
  let pending: number[] = [];
  let running = false;

  const later = (ms: number, run: () => void) => {
    pending.push(window.setTimeout(run, ms));
  };

  // Hidden lines keep their text, so the window holds its height while the demo plays.
  const reset = () => {
    lines.forEach((line) => {
      line.classList.add("live-hidden");
      line.classList.remove("live-typing");
    });
    items.forEach((item) => item.classList.add("live-hidden"));
  };

  const type = (index: number, done: () => void) => {
    const line = lines[index];
    const text = texts[index];
    if (!line || !text) return done();
    text.textContent = "";
    line.classList.remove("live-hidden");
    line.classList.add("live-typing");
    let shown = 0;
    const step = () => {
      shown += 1;
      text.textContent = full[index].slice(0, shown);
      if (shown < full[index].length) later(26 + Math.random() * 40, step);
      else {
        line.classList.remove("live-typing");
        done();
      }
    };
    later(380, step);
  };

  const play = () => {
    reset();
    let index = 0;
    const next = () => {
      if (index >= lines.length) {
        later(500, () => items[2]?.classList.remove("live-hidden"));
        later(5200, play);
        return;
      }
      const current = index;
      type(current, () => {
        const item = produces[current];
        if (item !== null && items[item]) later(260, () => items[item].classList.remove("live-hidden"));
        index += 1;
        later(650, next);
      });
    };
    later(600, next);
  };

  whileVisible(
    device,
    () => {
      if (running) return;
      running = true;
      clock = window.setInterval(() => {
        seconds += 1;
        if (timer) timer.textContent = `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
      }, 1000);
      play();
    },
    () => {
      if (!running) return;
      running = false;
      window.clearInterval(clock);
      pending.forEach((id) => window.clearTimeout(id));
      pending = [];
      // Left finished rather than half-typed, in case it is the last thing someone saw.
      lines.forEach((line, index) => {
        line.classList.remove("live-hidden", "live-typing");
        const text = texts[index];
        if (text) text.textContent = full[index];
      });
      items.forEach((item) => item.classList.remove("live-hidden"));
    },
  );
}

// ---------------------------------------------------------------- particles

/**
 * Specks rising slowly behind the hero, like sound leaving a room. A canvas, paused whenever the
 * hero is off screen, and sized for the display's pixel density so the dots stay sharp.
 */
export function particles(): void {
  const canvas = document.querySelector<HTMLCanvasElement>("[data-particles]");
  const context = canvas?.getContext("2d");
  if (!MOTION || !canvas || !context) return;

  interface Speck {
    x: number;
    y: number;
    radius: number;
    speed: number;
    sway: number;
    phase: number;
    hue: "accent" | "record";
  }

  let width = 0;
  let height = 0;
  let specks: Speck[] = [];
  let frame = 0;
  let last = 0;

  const spawn = (anywhere: boolean): Speck => ({
    x: Math.random() * width,
    y: anywhere ? Math.random() * height : height + 10,
    radius: 0.6 + Math.random() * 1.8,
    speed: 8 + Math.random() * 22,
    sway: 6 + Math.random() * 18,
    phase: Math.random() * Math.PI * 2,
    hue: Math.random() < 0.18 ? "record" : "accent",
  });

  const size = () => {
    const ratio = Math.min(2, window.devicePixelRatio || 1);
    width = canvas.clientWidth;
    height = canvas.clientHeight;
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);
    context.setTransform(ratio, 0, 0, ratio, 0, 0);
    // Density scales with area, so a phone is not a snow globe and a wide screen is not empty.
    const count = Math.round(Math.min(90, (width * height) / 16000));
    specks = Array.from({ length: count }, () => spawn(true));
  };

  const draw = (now: number) => {
    const dt = Math.min(0.05, (now - last) / 1000);
    last = now;
    context.clearRect(0, 0, width, height);

    for (let index = 0; index < specks.length; index += 1) {
      const speck = specks[index];
      speck.y -= speck.speed * dt;
      speck.phase += dt * 0.8;
      if (speck.y < -10) specks[index] = spawn(false);

      // Brightest in the middle of the climb, fading in at the bottom and out at the top.
      const life = Math.max(0, Math.min(1, speck.y / height));
      const alpha = Math.sin(life * Math.PI) * (0.35 + 0.35 * Math.sin(speck.phase * 2.3));
      const x = speck.x + Math.sin(speck.phase) * speck.sway;

      context.beginPath();
      context.arc(x, speck.y, speck.radius, 0, Math.PI * 2);
      context.fillStyle =
        speck.hue === "accent" ? `rgba(165, 176, 250, ${alpha.toFixed(3)})` : `rgba(226, 56, 46, ${(alpha * 0.8).toFixed(3)})`;
      context.fill();
    }

    frame = requestAnimationFrame(draw);
  };

  size();
  window.addEventListener("resize", size);
  whileVisible(
    canvas,
    () => {
      cancelAnimationFrame(frame);
      last = performance.now();
      frame = requestAnimationFrame(draw);
    },
    () => cancelAnimationFrame(frame),
  );
}

// ---------------------------------------------------------------- decode

const GLYPHS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789#%&*+=<>/";

/**
 * Section labels resolve out of random characters the first time they appear, settling one
 * letter at a time from the left. Spaces stay spaces, so the label keeps its shape.
 */
export function decodeLabels(): void {
  const labels = [...document.querySelectorAll<HTMLElement>("[data-scramble]")];
  if (!MOTION || labels.length === 0 || !("IntersectionObserver" in window)) return;

  const decode = (label: HTMLElement) => {
    const target = label.textContent ?? "";
    // Fixed width while it runs, so neighbouring text does not shuffle as glyphs change.
    label.style.minWidth = `${label.offsetWidth}px`;
    label.setAttribute("aria-label", target);
    const started = performance.now();
    const duration = 450 + target.length * 28;

    const step = (now: number) => {
      const progress = Math.min(1, (now - started) / duration);
      const settled = Math.floor(progress * target.length);
      label.textContent = [...target]
        .map((char, index) => {
          if (index < settled || char === " ") return char;
          return GLYPHS[Math.floor(Math.random() * GLYPHS.length)];
        })
        .join("");
      if (progress < 1) requestAnimationFrame(step);
      else {
        label.textContent = target;
        label.style.minWidth = "";
        label.removeAttribute("aria-label");
      }
    };
    requestAnimationFrame(step);
  };

  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        observer.unobserve(entry.target);
        decode(entry.target as HTMLElement);
      }
    },
    { threshold: 1 },
  );
  labels.forEach((label) => observer.observe(label));
}

// ---------------------------------------------------------------- hover

/**
 * Feature cards light up under the pointer: a glow that follows it, and an icon that does its
 * own small move. Mouse and trackpad only.
 */
export function cardHover(scope: ParentNode = document): void {
  if (!MOTION || !FINE_POINTER) return;
  for (const card of scope.querySelectorAll<HTMLElement>(".feature")) {
    card.addEventListener("pointermove", (event) => {
      const box = card.getBoundingClientRect();
      card.style.setProperty("--cx", `${event.clientX - box.left}px`);
      card.style.setProperty("--cy", `${event.clientY - box.top}px`);
    });
  }
}

// ---------------------------------------------------------------- click

/**
 * The download button answers a press with a ripple from where it was pressed and a small burst
 * of particles. The download itself is untouched: nothing here calls `preventDefault`.
 */
export function downloadBurst(scope: ParentNode = document): void {
  if (!MOTION) return;

  scope.addEventListener("pointerdown", (event) => {
    const target = event.target as Element | null;
    const button = target?.closest<HTMLElement>(".dl-button.is-primary, .btn-primary");
    if (!button) return;

    const box = button.getBoundingClientRect();
    const pointer = event as PointerEvent;
    const x = pointer.clientX - box.left;
    const y = pointer.clientY - box.top;

    const ripple = document.createElement("span");
    ripple.className = "ripple";
    ripple.style.left = `${x}px`;
    ripple.style.top = `${y}px`;
    button.append(ripple);
    ripple.addEventListener("animationend", () => ripple.remove());

    // The burst only for the real download, where it reads as "on its way".
    if (!button.classList.contains("dl-button")) return;
    const burst = document.createElement("span");
    burst.className = "burst";
    burst.style.left = `${pointer.clientX}px`;
    burst.style.top = `${pointer.clientY}px`;
    for (let index = 0; index < 14; index += 1) {
      const angle = (index / 14) * Math.PI * 2 + Math.random() * 0.4;
      const distance = 40 + Math.random() * 50;
      const bit = document.createElement("i");
      bit.style.setProperty("--dx", `${(Math.cos(angle) * distance).toFixed(1)}px`);
      bit.style.setProperty("--dy", `${(Math.sin(angle) * distance).toFixed(1)}px`);
      bit.style.animationDelay = `${(Math.random() * 60).toFixed(0)}ms`;
      if (index % 3 === 0) bit.className = "is-record";
      burst.append(bit);
    }
    document.body.append(burst);
    window.setTimeout(() => burst.remove(), 900);
  });
}
