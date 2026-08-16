import { useCallback, useEffect, useState } from "react";

import { api } from "./api";
import { applyTheme, DEFAULT_THEME, type Mode, type Theme } from "./theme";

/**
 * The theme, applied immediately and remembered by the engine.
 *
 * Not `localStorage`. The desktop shell binds port 0, so the window's origin changes on every
 * launch and anything kept per-origin is gone by the next one — a theme that resets each time
 * the app opens is not a theme. The engine keeps it beside the other preferences.
 *
 * Applied optimistically: the paint happens on click, and the write follows. A theme that waited
 * on a round trip would feel broken on a machine doing anything else.
 */
export function useTheme(): {
  theme: Theme;
  setMode: (mode: Mode) => void;
  setAccent: (accent: string) => void;
} {
  const [theme, setTheme] = useState<Theme>(DEFAULT_THEME);

  useEffect(() => {
    let cancelled = false;
    void api
      .preferences()
      .then((prefs) => {
        if (cancelled) return;
        const loaded: Theme = {
          mode: (prefs.mode as Mode) ?? DEFAULT_THEME.mode,
          accent: (prefs.accent as string) ?? DEFAULT_THEME.accent,
        };
        setTheme(loaded);
        applyTheme(loaded);
      })
      // An unreachable engine is not a reason to be unstyled.
      .catch(() => applyTheme(DEFAULT_THEME));

    applyTheme(DEFAULT_THEME);
    return () => {
      cancelled = true;
    };
  }, []);

  // `system` has to keep tracking the OS after the first paint, or it is just a starting value.
  useEffect(() => {
    if (theme.mode !== "system") return;
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => applyTheme(theme);
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [theme]);

  const update = useCallback((next: Theme) => {
    setTheme(next);
    applyTheme(next);
    // Failure is silent on purpose: the theme is already applied, and an error banner over a
    // colour that did not save would be louder than the problem.
    void api.setPreferences({ mode: next.mode, accent: next.accent }).catch(() => {});
  }, []);

  return {
    theme,
    setMode: useCallback((mode: Mode) => update({ ...theme, mode }), [theme, update]),
    setAccent: useCallback((accent: string) => update({ ...theme, accent }), [theme, update]),
  };
}
