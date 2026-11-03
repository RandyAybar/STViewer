import { getCurrentWindow } from "@tauri-apps/api/window";

/** Display preferences, persisted in localStorage and applied live. */

export type Theme = "system" | "light" | "dark";

/** Also read by `public/theme.js` before first paint; keep the key and values in sync. */
const THEME_KEY = "stv.theme";
const TZ_KEY = "stv.timeZone";

/** The OS / WebView time zone, e.g. `Europe/Berlin`. */
export const SYSTEM_TIME_ZONE = new Intl.DateTimeFormat().resolvedOptions().timeZone;

/** Every IANA zone the WebView knows, plus `UTC` (not always listed). */
export const TIME_ZONES: string[] = (() => {
  const zones = Intl.supportedValuesOf("timeZone");
  return zones.includes("UTC") ? zones : ["UTC", ...zones];
})();

function stored<T extends string>(key: string, valid: (v: string) => boolean, fallback: T): T {
  const v = localStorage.getItem(key);
  return v != null && valid(v) ? (v as T) : fallback;
}

const dark = window.matchMedia("(prefers-color-scheme: dark)");

class Settings {
  /** IANA zone used for every date shown; `""` = the system zone. */
  timeZone = $state(stored<string>(TZ_KEY, (v) => TIME_ZONES.includes(v), ""));
  theme = $state<Theme>(stored<Theme>(THEME_KEY, (v) => v === "light" || v === "dark" || v === "system", "system"));
  /** `prefers-color-scheme: dark` while following the system. */
  #systemDark = $state(dark.matches);

  /** The zone dates are formatted in. */
  get zone(): string {
    return this.timeZone || SYSTEM_TIME_ZONE;
  }

  /** Light or dark, after resolving "system". */
  get resolvedTheme(): "light" | "dark" {
    return this.theme === "system" ? (this.#systemDark ? "dark" : "light") : this.theme;
  }

  constructor() {
    dark.addEventListener("change", (e) => {
      this.#systemDark = e.matches;
      this.#applyTheme();
    });
    this.#applyTheme();
    void this.#applyWindowTheme();
  }

  setTimeZone(zone: string): void {
    this.timeZone = zone;
    if (zone) localStorage.setItem(TZ_KEY, zone);
    else localStorage.removeItem(TZ_KEY);
  }

  setTheme(theme: Theme): void {
    this.theme = theme;
    localStorage.setItem(THEME_KEY, theme);
    this.#applyTheme();
    void this.#applyWindowTheme();
  }

  /** `data-theme` on <html> selects the palette in app.css (also set early by theme.js). */
  #applyTheme(): void {
    document.documentElement.dataset.theme = this.resolvedTheme;
  }

  /** Native title bar / frame; `null` follows the OS (and with it `prefers-color-scheme`). */
  async #applyWindowTheme(): Promise<void> {
    try {
      await getCurrentWindow().setTheme(this.theme === "system" ? null : this.theme);
    } catch (e) {
      console.warn("setTheme failed", e);
      return;
    }
    // A forced window theme also forces the WebView's media query; re-read it once released.
    if (this.theme === "system") {
      this.#systemDark = dark.matches;
      this.#applyTheme();
    }
  }
}

export const settings = new Settings();
