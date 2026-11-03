import { openUrl } from "@tauri-apps/plugin-opener";
import { errMsg } from "./api";
import { store } from "./store.svelte";

const SAFE_SCHEME = /^(https?:|mailto:)/i;

/** Web and mail links leave the app; everything else (file:, relative, javascript:…) does not. */
export function canOpenExternal(href: string): boolean {
  return SAFE_SCHEME.test(href);
}

/** Opens `href` in the OS browser / mail client when [`canOpenExternal`]; ignores it otherwise. */
export function openExternal(href: string): void {
  if (canOpenExternal(href)) openUrl(href).catch((e: unknown) => store.toast(errMsg(e), "error"));
}

/**
 * `mailto:` link for one address. The address is percent-encoded (RFC 6068) so a stored
 * value like `a@example.org?attach=…` cannot add headers or attachments to the draft.
 */
export function mailtoHref(email: string): string {
  return `mailto:${encodeURIComponent(email.trim()).replace(/%40|%2B/g, decodeURIComponent)}`;
}

/**
 * Link for a stored web address: a bare `www.example.org` or `example.org:8080/x` opens
 * as https; a value with its own scheme is kept for [`canOpenExternal`] to judge.
 */
export function webHref(url: string): string {
  const u = url.trim();
  return /^[a-z][a-z\d+.-]*:(?!\d)/i.test(u) ? u : `https://${u}`;
}
