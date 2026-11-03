import { STV_CSP_SOURCE, cidUrl } from "./api";
import type { Locator, SourceId } from "./types";

export interface PreparedHtml {
  /** Full document for `<iframe srcdoc>`. */
  doc: string;
  /** Remote (http/https) resources found; blocked unless `allowRemote`. */
  remoteCount: number;
  /** Content-IDs referenced via `cid:` (those attachments are shown inline, not in the bar). */
  usedCids: Set<string>;
}

const REMOTE_RE = /^\s*(https?:)?\/\//i;
const CSS_URL_RE = /url\(\s*(['"]?)([^'")]+)\1\s*\)/gi;

export function normalizeCid(cid: string): string {
  return cid.trim().replace(/^cid:/i, "").replace(/^<|>$/g, "").toLowerCase();
}

/**
 * Turns an untrusted HTML mail body into a self-contained srcdoc:
 * - parsed with DOMParser (never executed), scripts/frames/forms/event handlers removed;
 * - `cid:` references rewritten to the `stv://` protocol;
 * - a CSP meta that forbids scripts and, unless `allowRemote`, any network fetch.
 * The iframe is additionally `sandbox`ed without `allow-scripts`.
 */
export function prepareHtml(html: string, source: SourceId, locator: Locator, allowRemote: boolean): PreparedHtml {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const usedCids = new Set<string>();
  let remoteCount = 0;

  for (const el of doc.querySelectorAll(
    "script, iframe, frame, frameset, object, embed, applet, base, form, meta[http-equiv], link:not([rel~=stylesheet i])",
  )) {
    el.remove();
  }

  const rewriteUrl = (url: string): string | null => {
    if (/^\s*cid:/i.test(url)) {
      const cid = normalizeCid(url);
      usedCids.add(cid);
      return cidUrl(source, locator, cid);
    }
    if (REMOTE_RE.test(url)) {
      remoteCount++;
      return allowRemote ? url : null;
    }
    return url;
  };
  const rewriteCss = (css: string): string =>
    css.replace(CSS_URL_RE, (whole, _q: string, url: string) => {
      const r = rewriteUrl(url);
      return r == null ? "none" : r === url ? whole : `url("${r}")`;
    });

  for (const el of doc.querySelectorAll("*")) {
    for (const attr of [...el.attributes]) {
      const name = attr.name.toLowerCase();
      if (name.startsWith("on") || name === "formaction" || name === "action") el.removeAttribute(attr.name);
      else if ((name === "href" || name === "xlink:href") && /^\s*(javascript|vbscript|data):/i.test(attr.value))
        el.removeAttribute(attr.name);
    }
    const style = el.getAttribute("style");
    if (style && /url\(/i.test(style)) el.setAttribute("style", rewriteCss(style));
    if (el.hasAttribute("background")) {
      const r = rewriteUrl(el.getAttribute("background")!);
      if (r == null) el.removeAttribute("background");
      else el.setAttribute("background", r);
    }
  }

  for (const el of doc.querySelectorAll("img[src], input[src], video[poster], image")) {
    const attr = el.hasAttribute("src") ? "src" : el.hasAttribute("poster") ? "poster" : "href";
    const v = el.getAttribute(attr);
    if (v == null) continue;
    const r = rewriteUrl(v);
    if (r == null) {
      el.removeAttribute(attr);
      el.setAttribute("data-stv-blocked", "");
    } else if (r !== v) el.setAttribute(attr, r);
  }
  for (const el of doc.querySelectorAll("img[srcset], source[srcset]")) {
    if (REMOTE_RE.test(el.getAttribute("srcset") ?? "")) {
      remoteCount++;
      if (!allowRemote) el.removeAttribute("srcset");
    }
  }
  for (const el of doc.querySelectorAll("style")) {
    if (el.textContent && /url\(/i.test(el.textContent)) el.textContent = rewriteCss(el.textContent);
  }
  for (const el of doc.querySelectorAll("link[rel~=stylesheet i]")) {
    remoteCount++;
    if (!allowRemote) el.remove();
  }

  const remote = allowRemote ? " https: http:" : "";
  const csp = doc.createElement("meta");
  csp.setAttribute("http-equiv", "Content-Security-Policy");
  csp.setAttribute(
    "content",
    `default-src 'none'; script-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'; ` +
      `img-src ${STV_CSP_SOURCE} data:${remote}; style-src 'unsafe-inline'${remote}; font-src data:${remote}; media-src ${STV_CSP_SOURCE}${remote}`,
  );
  // Chromium resolves link hosts ahead of a click; that alone tells a sender the mail was opened.
  const noPrefetch = doc.createElement("meta");
  noPrefetch.setAttribute("http-equiv", "x-dns-prefetch-control");
  noPrefetch.setAttribute("content", "off");
  const base = doc.createElement("style");
  base.textContent =
    "html{color-scheme:light}body{margin:16px;font:14px/1.45 system-ui,'Segoe UI',sans-serif;color:#1b1b1f;background:#fff;overflow-wrap:anywhere}" +
    "img[data-stv-blocked]{outline:1px dashed #bbb;min-width:16px;min-height:16px}pre{white-space:pre-wrap}";
  const head = doc.head ?? doc.documentElement.insertBefore(doc.createElement("head"), doc.body);
  head.prepend(csp, noPrefetch, base);

  return { doc: "<!doctype html>" + doc.documentElement.outerHTML, remoteCount, usedCids };
}
