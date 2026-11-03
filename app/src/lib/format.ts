import { settings } from "./settings.svelte";
import type { Address, Locator, When } from "./types";

/** macOS: shortcuts use ⌘, and the open panel has no file-type menu. */
export const IS_MAC = /Mac/.test(navigator.userAgent);

/** Tooltip label for a Ctrl/⌘ shortcut: `⌘F` on macOS, `Ctrl+F` elsewhere. */
export function shortcut(key: string): string {
  return IS_MAC ? `⌘${key}` : `Ctrl+${key}`;
}

type DateStyle = "long" | "list" | "full" | "hop";

const DATE_STYLES: Record<DateStyle, Intl.DateTimeFormatOptions> = {
  /** Reading pane header: `Wed, Jan 31, 2025, 5:12 PM`. */
  long: { weekday: "short", year: "numeric", month: "short", day: "numeric", hour: "numeric", minute: "2-digit" },
  /** List cells: locale date with 4-digit year plus time, e.g. `10/24/2001, 3:12 PM`. */
  list: { year: "numeric", month: "2-digit", day: "2-digit", hour: "numeric", minute: "2-digit" },
  /** Tooltips: full date, time with seconds, and zone offset. */
  full: { dateStyle: "full", timeStyle: "long" },
  /** Delivery-path hops: `Jan 31, 5:12:03 PM`. */
  hop: { month: "short", day: "numeric", hour: "numeric", minute: "2-digit", second: "2-digit" },
};

/** Formatters per style for the zone they were built for (rebuilt when the setting changes). */
let cache: { zone: string; fmts: Partial<Record<DateStyle, Intl.DateTimeFormat>> } = { zone: "", fmts: {} };

/**
 * Formatter for the display time zone chosen in Settings. Reading `settings.zone` here
 * makes every caller in a template re-render when the zone changes.
 */
function formatter(style: DateStyle): Intl.DateTimeFormat {
  const zone = settings.zone;
  if (cache.zone !== zone) cache = { zone, fmts: {} };
  return (cache.fmts[style] ??= new Intl.DateTimeFormat(undefined, { ...DATE_STYLES[style], timeZone: zone }));
}

function formatDate(ms: number | null, style: DateStyle): string {
  return ms == null ? "" : formatter(style).format(ms);
}

export function longDate(ms: number | null): string {
  return formatDate(ms, "long");
}

/** Date column of message and attachment lists; empty when the message has no date. */
export function dateTime(ms: number | null): string {
  return formatDate(ms, "list");
}

/** Tooltip text: full date and time plus the display zone, e.g. `… 5:12:03 PM GMT+9 (Asia/Tokyo)`. */
export function fullDateTime(ms: number | null): string {
  return ms == null ? "" : `${formatDate(ms, "full")} (${settings.zone})`;
}

/** Time of one `Received:` hop. */
export function hopTime(ms: number | null): string {
  return formatDate(ms, "hop");
}

type DayStyle = "long" | "list" | "full";

const DAY_STYLES: Record<DayStyle, Intl.DateTimeFormatOptions> = {
  long: { weekday: "short", year: "numeric", month: "short", day: "numeric" },
  list: { year: "numeric", month: "2-digit", day: "2-digit" },
  full: { dateStyle: "full" },
};

/**
 * Calendar days (all-day items, birthdays, task dates) are formatted in UTC from their own
 * midnight, so no display zone can move them to the neighbouring day.
 */
const dayFmts: Partial<Record<DayStyle, Intl.DateTimeFormat>> = {};

function dayFormatter(style: DayStyle): Intl.DateTimeFormat {
  return (dayFmts[style] ??= new Intl.DateTimeFormat(undefined, { ...DAY_STYLES[style], timeZone: "UTC" }));
}

const DAY_MS = 86_400_000;

/** `YYYY-MM-DD` → UTC midnight of that day; `null` when malformed or not a real date. */
function dayMs(day: string): number | null {
  const m = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(day);
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]) - 1, Number(m[3])];
  // `setUTCFullYear`, not `Date.UTC`: the latter maps years 0–99 to 1900–1999.
  const t = new Date(0);
  t.setUTCFullYear(y, mo, d);
  return t.getUTCMonth() === mo && t.getUTCDate() === d ? t.getTime() : null;
}

/** UTC midnight → `YYYY-MM-DD`. */
function isoDay(ms: number): string {
  const t = new Date(ms);
  const pad = (n: number, w: number) => String(n).padStart(w, "0");
  return `${pad(t.getUTCFullYear(), 4)}-${pad(t.getUTCMonth() + 1, 2)}-${pad(t.getUTCDate(), 2)}`;
}

function formatWhenAs(w: When | null | undefined, time: DateStyle, day: DayStyle): string {
  if (!w) return "";
  if (w.kind === "time") return formatDate(w.value, time);
  const ms = dayMs(w.value);
  return ms == null ? w.value : dayFormatter(day).format(ms);
}

/** Reading pane: `Tue, Jan 31, 2025, 9:00 AM`, or `Tue, Jan 31, 2025` for a calendar day. */
export function formatWhen(w: When | null | undefined): string {
  return formatWhenAs(w, "long", "long");
}

/** List cells: same shape as [`dateTime`], date only for a calendar day. */
export function whenList(w: When | null | undefined): string {
  return formatWhenAs(w, "list", "list");
}

/** Tooltip: [`fullDateTime`] for a time; the full date (no zone) for a calendar day. */
export function whenFull(w: When | null | undefined): string {
  return w?.kind === "time" ? fullDateTime(w.value) : formatWhenAs(w, "full", "full");
}

/** All-day ends are exclusive (the next midnight): the last day the item covers. Times pass through. */
export function lastDay(end: When | null | undefined): When | null {
  if (end?.kind !== "date") return end ?? null;
  const ms = dayMs(end.value);
  return ms == null ? end : { kind: "date", value: isoDay(ms - DAY_MS) };
}

/**
 * Appointment span: `Tue, Jan 31, 2025, 9:00 – 10:30 AM` on one day, both ends spelled
 * out across days. All-day ends are exclusive (stored as the next midnight), so the last
 * day shown is the one before; a one-day event shows just its date.
 */
export function whenRange(start: When | null, end: When | null): string {
  if (!start || !end) return formatWhen(start ?? end);
  if (start.kind === "date" && end.kind === "date") {
    const a = dayMs(start.value);
    const b = dayMs(end.value);
    if (a == null || b == null) return `${formatWhen(start)} – ${formatWhen(end)}`;
    const last = Math.max(a, b - DAY_MS);
    return last === a ? dayFormatter("long").format(a) : dayFormatter("long").formatRange(a, last);
  }
  if (start.kind === "time" && end.kind === "time") {
    return end.value > start.value ? formatter("long").formatRange(start.value, end.value) : formatWhen(start);
  }
  return `${formatWhen(start)} – ${formatWhen(end)}`;
}

export function size(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

export function addrShort(a: Address | null | undefined): string {
  if (!a) return "";
  return a.name?.trim() || a.email || "";
}

/** `Name <email>`; just the name when it already spells the address (Outlook's `Name (email)` display names). */
export function addrFull(a: Address): string {
  const name = a.name?.trim();
  if (name && a.email && !name.toLowerCase().includes(a.email.toLowerCase())) return `${name} <${a.email}>`;
  return name || a.email || "";
}

/** Up to two initials for an avatar, from the display name or address. */
export function initials(a: Address | null | undefined): string {
  const parts = addrShort(a)
    .replace(/[<>"']/g, "")
    .split(/[\s@.]+/)
    .filter(Boolean);
  return ((parts[0]?.[0] ?? "?") + (parts[1]?.[0] ?? "")).toUpperCase();
}

/** Stable avatar hue (0–359) per sender. */
export function avatarHue(a: Address | null | undefined): number {
  const s = a?.email ?? a?.name ?? "";
  let x = 0;
  for (let i = 0; i < s.length; i++) x = (x * 31 + s.charCodeAt(i)) | 0;
  return Math.abs(x) % 360;
}

export function sameLocator(a: Locator | null | undefined, b: Locator | null | undefined): boolean {
  if (!a || !b) return false;
  return JSON.stringify(a) === JSON.stringify(b);
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) =>
    c === "&" ? "&amp;" : c === "<" ? "&lt;" : c === ">" ? "&gt;" : c === '"' ? "&quot;" : "&#39;",
  );
}

const LINK_RE = /\b(https?:\/\/[^\s<>"']+[^\s<>"'.,;:!?)\]}]|mailto:[^\s<>"']+|[\w.+-]+@[\w-]+(?:\.[\w-]+)+)/gi;

/**
 * Plain text → safe HTML with links and dimmed quote lines. Links carry `data-href`
 * so clicks are routed through the opener plugin instead of navigating the webview.
 */
export function linkify(text: string): string {
  const out: string[] = [];
  for (const line of text.split(/\r?\n/)) {
    let html = "";
    let last = 0;
    for (const m of line.matchAll(LINK_RE)) {
      const raw = m[0];
      const idx = m.index ?? 0;
      html += escapeHtml(line.slice(last, idx));
      const href = /^(https?:|mailto:)/i.test(raw) ? raw : `mailto:${raw}`;
      html += `<a href="#" data-href="${escapeHtml(href)}" title="${escapeHtml(href)}">${escapeHtml(raw)}</a>`;
      last = idx + raw.length;
    }
    html += escapeHtml(line.slice(last));
    out.push(/^\s*>/.test(line) ? `<span class="quote">${html}</span>` : html);
  }
  return out.join("\n");
}

/**
 * Best-effort RTF → text for bodies that only exist as RTF: drops destination groups
 * (font/color tables, `\*` groups), maps `\par`/`\line`/`\tab`, decodes `\'hh` (cp1252)
 * and `\uN` escapes.
 */
export function rtfToText(rtf: string): string {
  const cp1252 = new TextDecoder("windows-1252");
  const skipDest: Record<string, true> = Object.fromEntries(
    [
      "fonttbl",
      "colortbl",
      "stylesheet",
      "info",
      "pict",
      "header",
      "footer",
      "object",
      "themedata",
      "colorschememapping",
      "datastore",
      "latentstyles",
      "listtable",
      "listoverridetable",
      "rsidtbl",
      "generator",
      "xmlnstbl",
      "mmathPr",
    ].map((k) => [k, true] as const),
  );
  let out = "";
  let i = 0;
  // Stack of "skip" flags per group; skipping is inherited.
  const stack: boolean[] = [];
  let skip = false;
  let ucSkip = 1;
  let pendingSkip = 0;
  while (i < rtf.length) {
    const c = rtf[i];
    if (c === "{") {
      stack.push(skip);
      i++;
      continue;
    }
    if (c === "}") {
      skip = stack.pop() ?? false;
      i++;
      continue;
    }
    if (c === "\\") {
      const next = rtf[i + 1];
      if (next === "\\" || next === "{" || next === "}") {
        if (!skip) out += next;
        i += 2;
        continue;
      }
      if (next === "*") {
        skip = true;
        i += 2;
        continue;
      }
      if (next === "'") {
        const hex = rtf.slice(i + 2, i + 4);
        if (pendingSkip > 0) pendingSkip--;
        else if (!skip) out += cp1252.decode(new Uint8Array([parseInt(hex, 16) || 0x3f]));
        i += 4;
        continue;
      }
      const m = /^\\([a-zA-Z]+)(-?\d+)? ?/.exec(rtf.slice(i, i + 40));
      if (!m) {
        i += 2;
        continue;
      }
      const word = m[1];
      const arg = m[2];
      i += m[0].length;
      if (Object.hasOwn(skipDest, word)) skip = true;
      if (skip) continue;
      switch (word) {
        case "par":
        case "line":
        case "sect":
        case "page":
          out += "\n";
          break;
        case "tab":
          out += "\t";
          break;
        case "emdash":
          out += "\u2014";
          break;
        case "endash":
          out += "\u2013";
          break;
        case "bullet":
          out += "\u2022";
          break;
        case "lquote":
          out += "\u2018";
          break;
        case "rquote":
          out += "\u2019";
          break;
        case "ldblquote":
          out += "\u201c";
          break;
        case "rdblquote":
          out += "\u201d";
          break;
        case "uc":
          ucSkip = Number(arg ?? 1);
          break;
        case "u": {
          let n = Number(arg ?? 0);
          if (n < 0) n += 65536;
          out += String.fromCharCode(n);
          pendingSkip = ucSkip;
          break;
        }
      }
      continue;
    }
    if (c === "\r" || c === "\n") {
      i++;
      continue;
    }
    if (pendingSkip > 0) pendingSkip--;
    else if (!skip) out += c;
    i++;
  }
  return out.replace(/\n{3,}/g, "\n\n").trim();
}

const BIDI_CONTROLS = /[\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

/**
 * Attachment file name for display. Bidi controls are shown as `[U+202E]` instead of
 * applied, so `invoice\u202Efdp.exe` cannot pose as `invoiceexe.pdf`.
 */
export function fileName(name: string): string {
  return name.replace(BIDI_CONTROLS, (c) => `[U+${c.charCodeAt(0).toString(16).toUpperCase().padStart(4, "0")}]`);
}
