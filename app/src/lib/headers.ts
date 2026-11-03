// Parsing of a message's raw internet headers for the Headers tab.

export interface RawHeader {
  name: string;
  value: string;
}

/** Unfolds continuation lines and decodes RFC 2047 encoded words. */
export function parseHeaders(raw: string): RawHeader[] {
  const out: RawHeader[] = [];
  for (const line of raw.split(/\r?\n/)) {
    if (/^[ \t]/.test(line) && out.length) {
      out[out.length - 1].value += " " + line.trim();
      continue;
    }
    const i = line.indexOf(":");
    if (i <= 0) continue;
    out.push({ name: line.slice(0, i).trim(), value: line.slice(i + 1).trim() });
  }
  for (const h of out) h.value = decodeWords(h.value);
  return out;
}

/** First value of header `name` (case-insensitive). */
export function first(headers: RawHeader[], name: string): string | null {
  const n = name.toLowerCase();
  return headers.find((h) => h.name.toLowerCase() === n)?.value ?? null;
}

export function all(headers: RawHeader[], name: string): string[] {
  const n = name.toLowerCase();
  return headers.filter((h) => h.name.toLowerCase() === n).map((h) => h.value);
}

/** RFC 2047 `=?charset?B|Q?text?=`; whitespace between adjacent encoded words is dropped. */
function decodeWords(s: string): string {
  if (!s.includes("=?")) return s;
  return s
    .replace(/(\?=)\s+(=\?)/g, "$1$2")
    .replace(/=\?([^?]+)\?([BbQq])\?([^?]*)\?=/g, (whole, charset: string, enc: string, text: string) => {
      try {
        let bytes: Uint8Array;
        if (enc.toUpperCase() === "B") {
          bytes = Uint8Array.from(atob(text), (c) => c.charCodeAt(0));
        } else {
          const t = text.replace(/_/g, " ");
          const arr: number[] = [];
          for (let i = 0; i < t.length; i++) {
            if (t[i] === "=" && /^[0-9A-Fa-f]{2}$/.test(t.slice(i + 1, i + 3))) {
              arr.push(parseInt(t.slice(i + 1, i + 3), 16));
              i += 2;
            } else arr.push(t.charCodeAt(i));
          }
          bytes = Uint8Array.from(arr);
        }
        return new TextDecoder(charset.split("*")[0].toLowerCase()).decode(bytes);
      } catch {
        return whole;
      }
    });
}

/** One `Received:` hop. */
export interface Hop {
  from: string | null;
  fromDetail: string | null;
  by: string | null;
  protocol: string | null;
  tls: string | null;
  date: number | null;
}

/** `Received:` hops, oldest (first server) first. */
export function hops(headers: RawHeader[]): Hop[] {
  return all(headers, "Received").map(parseReceived).reverse();
}

function parseReceived(v: string): Hop {
  const semi = v.lastIndexOf(";");
  const route = semi >= 0 ? v.slice(0, semi) : v;
  const when =
    semi >= 0
      ? v
          .slice(semi + 1)
          .replace(/\([^)]*\)/g, "")
          .trim()
      : "";
  const t = when ? Date.parse(when) : NaN;
  const from = /\bfrom\s+(\S+)(?:\s+\(([^)]*)\))?/i.exec(route);
  const by = /\bby\s+(\S+)/i.exec(route);
  const withM = /\bwith\s+(.+?)(?=\s+(?:id|for|via)\b|\s*[(;]|$)/i.exec(route);
  const tls = /\bversion=(TLS|SSL)v?([\d_.]+)/i.exec(route) ?? /\b(TLS)v?(1[._]\d)\b/i.exec(route);
  let protocol = withM?.[1].trim() ?? null;
  if (protocol && /^Microsoft SMTP Server$/i.test(protocol)) protocol = "SMTP";
  if (protocol && /^mapi$/i.test(protocol)) protocol = "MAPI";
  const encrypted = tls
    ? `${tls[1].toUpperCase()} ${tls[2].replace("_", ".")}`
    : protocol && /^E?SMTPSA?$/i.test(protocol)
      ? "Encrypted"
      : null;
  return {
    from: from?.[1] ?? null,
    fromDetail: from?.[2]?.trim() || null,
    by: by?.[1] ?? null,
    protocol,
    tls: encrypted,
    date: Number.isNaN(t) ? null : t,
  };
}

export type Verdict = "pass" | "fail" | "warn" | "none";

export interface AuthCheck {
  method: string;
  result: string;
  verdict: Verdict;
}

const METHODS: Record<string, string> = { spf: "SPF", dkim: "DKIM", dmarc: "DMARC", compauth: "CompAuth", arc: "ARC" };

/** SPF/DKIM/DMARC results from `Authentication-Results` (else ARC or `Received-SPF`). */
export function authChecks(headers: RawHeader[]): AuthCheck[] {
  const src = first(headers, "Authentication-Results") ?? first(headers, "ARC-Authentication-Results") ?? "";
  const seen = new Map<string, AuthCheck>();
  for (const m of src.matchAll(/\b(spf|dkim|dmarc|compauth|arc)=([a-z]+)/gi)) {
    const method = METHODS[m[1].toLowerCase()];
    if (!seen.has(method)) seen.set(method, check(method, m[2]));
  }
  const rspf = first(headers, "Received-SPF");
  if (!seen.has("SPF") && rspf) {
    const r = /^\s*([a-z]+)/i.exec(rspf);
    if (r) seen.set("SPF", check("SPF", r[1]));
  }
  return [...seen.values()];
}

function check(method: string, raw: string): AuthCheck {
  const r = raw.toLowerCase();
  const verdict: Verdict =
    r === "pass" || r === "bestguesspass"
      ? "pass"
      : r === "fail" || r === "hardfail" || r === "permerror"
        ? "fail"
        : r === "softfail" || r === "temperror" || r === "neutral" || r === "policy"
          ? "warn"
          : "none";
  const result =
    {
      pass: "passed",
      bestguesspass: "passed",
      fail: "failed",
      hardfail: "failed",
      softfail: "soft fail",
      none: "not signed",
      neutral: "neutral",
      temperror: "temporary error",
      permerror: "error",
      policy: "policy",
    }[r] ?? r;
  return { method, result: method === "DKIM" || r !== "none" ? result : "not set", verdict };
}

/** Microsoft spam confidence level, or a SpamAssassin verdict. */
export function spam(headers: RawHeader[]): { label: string; verdict: Verdict } | null {
  const scl =
    first(headers, "X-MS-Exchange-Organization-SCL") ??
    /\bSCL:(-?\d)/.exec(first(headers, "X-Forefront-Antispam-Report") ?? "")?.[1];
  if (scl != null && /^-?\d$/.test(scl.trim())) {
    const n = Number(scl);
    if (n < 0) return { label: "Trusted sender (internal)", verdict: "pass" };
    if (n <= 1) return { label: `Not spam (SCL ${n})`, verdict: "pass" };
    if (n <= 4) return { label: `Possibly spam (SCL ${n})`, verdict: "warn" };
    return { label: `Spam (SCL ${n})`, verdict: "fail" };
  }
  const sa = first(headers, "X-Spam-Status") ?? first(headers, "X-Spam-Flag");
  if (sa) {
    const yes = /^\s*yes/i.test(sa);
    const score = /score=(-?[\d.]+)/i.exec(sa)?.[1];
    return { label: `${yes ? "Spam" : "Not spam"}${score ? ` (score ${score})` : ""}`, verdict: yes ? "fail" : "pass" };
  }
  return null;
}

/** "2 s", "3 min", "1 h 4 min", "2 d". */
export function duration(ms: number): string {
  const s = Math.round(Math.abs(ms) / 1000);
  if (s < 60) return `${s} s`;
  const m = Math.round(s / 60);
  if (m < 60) return `${m} min`;
  const h = Math.floor(m / 60);
  if (h < 48) return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
  return `${Math.round(h / 24)} d`;
}

export type HeaderGroup = "message" | "delivery" | "other";

const DELIVERY =
  /^(received|return-path|delivered-to|authentication-results|arc-.*|dkim-signature|domainkey-signature|received-spf|x-received|x-original-to|envelope-to)$/i;

/** Message headers, delivery/authentication headers, and vendor (`X-`) and other extras. */
export function groupOf(name: string): HeaderGroup {
  if (DELIVERY.test(name)) return "delivery";
  if (/^(x-|thread-|accept-language|content-language)/i.test(name)) return "other";
  return "message";
}
