// Advanced-search form ⇄ search query string (syntax: crates/index/src/query.rs).

export type SizeUnit = "KB" | "MB";

/** `is:` values that pick non-mail items; `""` = any. */
export const ITEM_TYPES = ["appointment", "meeting", "contact", "task"] as const;
export type ItemType = (typeof ITEM_TYPES)[number] | "";

export interface AdvancedQuery {
  from: string;
  /** To, Cc or Bcc. */
  to: string;
  subject: string;
  /** Free text, plus anything the form has no field for. */
  words: string;
  phrase: string;
  /** Space-separated words to exclude. */
  without: string;
  attachment: string;
  /** `YYYY-MM-DD` (date input value). */
  after: string;
  before: string;
  larger: string;
  largerUnit: SizeUnit;
  smaller: string;
  smallerUnit: SizeUnit;
  hasAttachment: boolean;
  unread: boolean;
  flagged: boolean;
  itemType: ItemType;
  folder: string;
  label: string;
}

export function emptyAdvanced(): AdvancedQuery {
  return {
    from: "",
    to: "",
    subject: "",
    words: "",
    phrase: "",
    without: "",
    attachment: "",
    after: "",
    before: "",
    larger: "",
    largerUnit: "MB",
    smaller: "",
    smallerUnit: "KB",
    hasAttachment: false,
    unread: false,
    flagged: false,
    itemType: "",
    folder: "",
    label: "",
  };
}

/** Quotes an operator value when the query lexer would otherwise split it. */
export function quoteValue(v: string): string {
  return /[\s"():]/.test(v) ? `"${v.replace(/"/g, "")}"` : v;
}

/** Operator names the backend lexer recognises (others are plain words). */
const FIELDS: Record<string, true> = {
  from: true,
  to: true,
  cc: true,
  bcc: true,
  subject: true,
  body: true,
  attachment: true,
  filename: true,
  has: true,
  is: true,
  label: true,
  in: true,
  before: true,
  older: true,
  after: true,
  newer: true,
  larger: true,
  smaller: true,
};

interface Token {
  /** Source text, re-emitted verbatim when the form has no field for it. */
  raw: string;
  neg: boolean;
  field: string | null;
  value: string;
  quoted: boolean;
}

/** Same clause boundaries as the backend lexer; parentheses stay inside raw tokens. */
function lex(q: string): Token[] {
  const out: Token[] = [];
  const n = q.length;
  let i = 0;
  while (i < n) {
    if (/\s/.test(q[i])) {
      i++;
      continue;
    }
    const start = i;
    let neg = false;
    if (q[i] === "-" && i + 1 < n && !/\s/.test(q[i + 1])) {
      neg = true;
      i++;
    }
    let field: string | null = null;
    const m = /^([A-Za-z]+):(?=\S)/.exec(q.slice(i));
    if (m && Object.hasOwn(FIELDS, m[1].toLowerCase())) {
      field = m[1].toLowerCase();
      i += m[0].length;
    }
    let value = "";
    let quoted = false;
    if (q[i] === '"') {
      quoted = true;
      const end = q.indexOf('"', i + 1);
      value = q.slice(i + 1, end < 0 ? n : end);
      i = end < 0 ? n : end + 1;
    } else {
      while (i < n && !/\s/.test(q[i])) value += q[i++];
    }
    out.push({ raw: q.slice(start, i), neg, field, value, quoted });
  }
  return out;
}

/** `2022`, `2022-3`, `2022/03/07` → `2022-03-07` (first day for partial dates, like the backend). */
function normDate(v: string): string | null {
  const m = /^(\d{4})(?:[-/.](\d{1,2}))?(?:[-/.](\d{1,2}))?$/.exec(v);
  if (!m) return null;
  return `${m[1]}-${(m[2] ?? "1").padStart(2, "0")}-${(m[3] ?? "1").padStart(2, "0")}`;
}

/** `5M`, `100K`, `2MB`, `1G`, `1234` → number in KB or MB. */
function parseSize(v: string): { n: string; unit: SizeUnit } | null {
  const m = /^(\d+(?:\.\d+)?)\s*([kmg]?)b?$/i.exec(v);
  if (!m) return null;
  const n = Number(m[1]);
  const round = (x: number) => String(Math.round(x * 100) / 100);
  switch (m[2].toLowerCase()) {
    case "k":
      return { n: round(n), unit: "KB" };
    case "m":
      return { n: round(n), unit: "MB" };
    case "g":
      return { n: round(n * 1024), unit: "MB" };
    default:
      return { n: round(n / 1024), unit: "KB" };
  }
}

/** Best-effort split of `q` into form fields; anything else lands in `words`. */
export function parseAdvanced(q: string): AdvancedQuery {
  const f = emptyAdvanced();
  const words: string[] = [];
  const without: string[] = [];
  const subject: string[] = [];
  const single = (key: "from" | "to" | "attachment" | "folder" | "label", t: Token) => {
    if (f[key] || t.neg) words.push(t.raw);
    else f[key] = t.value;
  };
  // Parenthesised groups (`-(a b)`, `(x OR y)`) have no form equivalent: keep them verbatim.
  let depth = 0;
  for (const t of lex(q)) {
    const bare = t.raw.replace(/"[^"]*"?/g, "");
    const opens = bare.split("(").length - 1;
    const closes = bare.split(")").length - 1;
    if (depth > 0 || opens > 0 || closes > 0) {
      words.push(t.raw);
      depth = Math.max(0, depth + opens - closes);
      continue;
    }
    if (t.neg && !t.field && !t.quoted) {
      without.push(t.value);
      continue;
    }
    if (t.neg) {
      words.push(t.raw);
      continue;
    }
    switch (t.field) {
      case "from":
      case "to":
      case "label":
        single(t.field, t);
        break;
      case "attachment":
      case "filename":
        single("attachment", t);
        break;
      case "in":
        single("folder", t);
        break;
      case "subject":
        subject.push(t.quoted ? `"${t.value}"` : t.value);
        break;
      case "has":
        if (/^attachments?$/i.test(t.value)) f.hasAttachment = true;
        else words.push(t.raw);
        break;
      case "is": {
        const v = t.value.toLowerCase();
        if (v === "unread") f.unread = true;
        else if (v === "flagged" || v === "starred") f.flagged = true;
        else if (!f.itemType && (ITEM_TYPES as readonly string[]).includes(v)) f.itemType = v as ItemType;
        else words.push(t.raw);
        break;
      }
      case "after":
      case "newer":
      case "before":
      case "older": {
        const key = t.field === "after" || t.field === "newer" ? "after" : "before";
        const d = normDate(t.value);
        if (d && !f[key]) f[key] = d;
        else words.push(t.raw);
        break;
      }
      case "larger":
      case "smaller": {
        const s = parseSize(t.value);
        if (s && !f[t.field]) {
          f[t.field] = s.n;
          f[t.field === "larger" ? "largerUnit" : "smallerUnit"] = s.unit;
        } else words.push(t.raw);
        break;
      }
      case null:
        if (t.quoted && !f.phrase) f.phrase = t.value;
        else words.push(t.raw);
        break;
      default:
        words.push(t.raw);
    }
  }
  f.words = words.join(" ");
  f.without = without.join(" ");
  f.subject = subject.join(" ");
  return f;
}

/** Form fields → query string in the documented syntax. */
export function buildQuery(f: AdvancedQuery): string {
  const parts: string[] = [];
  const field = (name: string, v: string) => {
    const s = v.trim();
    if (s) parts.push(`${name}:${quoteValue(s)}`);
  };
  field("from", f.from);
  field("to", f.to);
  const subj = f.subject.trim();
  if (/^".+"$/.test(subj)) parts.push(`subject:"${subj.slice(1, -1).replace(/"/g, "")}"`);
  else for (const w of subj.split(/\s+/)) if (w) parts.push(`subject:${quoteValue(w)}`);
  if (f.words.trim()) parts.push(f.words.trim());
  const phrase = f.phrase.replace(/"/g, "").trim();
  if (phrase) parts.push(`"${phrase}"`);
  for (const w of f.without.split(/\s+/)) {
    const term = w.replace(/^-+/, "");
    if (term) parts.push(`-${quoteValue(term)}`);
  }
  field("attachment", f.attachment);
  if (f.hasAttachment) parts.push("has:attachment");
  if (f.unread) parts.push("is:unread");
  if (f.flagged) parts.push("is:flagged");
  if (f.itemType) parts.push(`is:${f.itemType}`);
  field("in", f.folder);
  field("label", f.label);
  if (f.after) parts.push(`after:${f.after}`);
  if (f.before) parts.push(`before:${f.before}`);
  const size = (op: string, v: string, unit: SizeUnit) => {
    const n = Number(v.trim().replace(",", "."));
    if (v.trim() && Number.isFinite(n) && n > 0) parts.push(`${op}:${n}${unit === "MB" ? "M" : "K"}`);
  };
  size("larger", f.larger, f.largerUnit);
  size("smaller", f.smaller, f.smallerUnit);
  return parts.join(" ");
}
