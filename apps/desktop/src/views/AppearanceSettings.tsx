import { Check, Monitor, Moon, Sun } from "lucide-react";

import { ACCENTS, type Mode, type Theme } from "../lib/theme";

interface Props {
  theme: Theme;
  onModeChange: (mode: Mode) => void;
  onAccentChange: (accent: string) => void;
}

const MODES: Array<{ id: Mode; label: string; Icon: typeof Sun; hint: string }> = [
  { id: "dark", label: "Dark", Icon: Moon, hint: "Default. Easier for long meetings." },
  { id: "light", label: "Light", Icon: Sun, hint: "For a bright room." },
  { id: "system", label: "System", Icon: Monitor, hint: "Follows macOS." },
];

/**
 * Theme controls.
 *
 * Applied on click rather than behind a Save button — a colour you cannot see until you confirm
 * it is a colour you have to pick twice. The write to the engine follows the paint.
 */
export function AppearanceSettings({ theme, onModeChange, onAccentChange }: Props) {
  return (
    <section>
      <h2 className="mb-1 text-[13px] font-semibold text-ink">Appearance</h2>
      <p className="mb-3 text-[12px] text-ink-muted">
        Kept by the engine rather than the browser, so it survives a restart.
      </p>

      <div className="mb-4 grid grid-cols-3 gap-2">
        {MODES.map(({ id, label, Icon, hint }) => {
          const active = theme.mode === id;
          return (
            <button
              key={id}
              type="button"
              onClick={() => onModeChange(id)}
              aria-pressed={active}
              className={`card flex flex-col items-start gap-1 px-3 py-2.5 text-left transition
                          ${active ? "border-accent" : "hover:bg-overlay"}`}
            >
              <span className="flex items-center gap-1.5 text-[13px] font-medium text-ink">
                <Icon size={14} aria-hidden />
                {label}
                {active && <Check size={13} className="text-accent" aria-hidden />}
              </span>
              <span className="text-[11px] leading-snug text-ink-faint">{hint}</span>
            </button>
          );
        })}
      </div>

      <p className="mb-2 text-[11px] font-medium uppercase tracking-wide text-ink-faint">
        Accent
      </p>
      <div className="flex flex-wrap gap-2">
        {ACCENTS.map((accent) => {
          const active = theme.accent === accent.id;
          return (
            <button
              key={accent.id}
              type="button"
              onClick={() => onAccentChange(accent.id)}
              aria-pressed={active}
              aria-label={accent.label}
              title={accent.label}
              className={`flex h-8 w-8 items-center justify-center rounded-full transition
                          ${active ? "ring-2 ring-offset-2" : "hover:scale-110"}`}
              style={{
                backgroundColor: accent.base,
                // `ring-offset` needs a colour that matches the surface behind it, which is a
                // variable rather than a Tailwind palette entry.
                ...(active
                  ? ({
                      // eslint-disable-next-line @typescript-eslint/consistent-type-assertions
                      "--tw-ring-color": accent.base,
                      "--tw-ring-offset-color": "var(--bg)",
                    } as React.CSSProperties)
                  : {}),
              }}
            >
              {active && <Check size={14} style={{ color: accent.on }} aria-hidden />}
            </button>
          );
        })}
      </div>
      <p className="mt-2 text-[11px] leading-snug text-ink-faint">
        Recording stays red in every theme. If it is red, audio is being captured — that should
        not depend on a colour you chose.
      </p>
    </section>
  );
}
