import { open, save } from "@tauri-apps/plugin-dialog";
import { api, errMsg } from "./api";
import { fileName } from "./format";
import { store } from "./store.svelte";
import type { AttachmentKind, AttachmentRow, Locator, SaveAllReport, SourceId } from "./types";

/** Attached e-mails (and TNEF) open in the reading pane as embedded messages, not with the OS. */
export function opensAsMessage(a: { kind: AttachmentKind; mime_type: string | null }): boolean {
  return a.kind === "embedded_message" || a.kind === "tnef" || a.mime_type?.toLowerCase() === "message/rfc822";
}

/** Image rows: served as themselves by the `att/` route (preview pane), thumbnails in the grid. */
export function isImageRow(r: AttachmentRow): boolean {
  return r.category === "image";
}

const DOCUMENT_CATEGORIES: Record<string, true> = {
  pdf: true,
  document: true,
  spreadsheet: true,
  presentation: true,
  text: true,
};

/** Documents: the `thumb/` / `preview/` routes may draw a page; otherwise a text card is shown. */
export function isDocumentRow(r: AttachmentRow): boolean {
  return Object.hasOwn(DOCUMENT_CATEGORIES, r.category);
}

/** Type column / badge text: extension, else MIME subtype. */
export function typeLabel(r: AttachmentRow): string {
  if (r.extension) return r.extension.toUpperCase();
  return r.mime_type?.split("/")[1]?.toUpperCase() ?? "";
}

/** File name safe for Windows, macOS and Linux file systems. */
function safeFileName(name: string): string {
  const s = name
    .replace(/[<>:"/\\|?*\u0000-\u001f]/g, "_")
    .replace(/[. ]+$/, "")
    .trim();
  if (!s) return "attachment";
  // Windows device names stay reserved with any extension (`con.txt`).
  return /^(con|prn|aux|nul|com\d|lpt\d)(\.|$)/i.test(s) ? `_${s}` : s;
}

/** Opens attachment `index` of a message with the OS default app; failures become a toast. */
export async function openAttachment(source: SourceId, message: Locator, index: number, name: string): Promise<void> {
  try {
    await api.openAttachment(source, message, index);
  } catch (e) {
    store.toast(`Could not open ${name}: ${errMsg(e)}`, "error");
  }
}

/** "Save as…" for attachment `index` of a message. */
export async function saveAttachmentAs(source: SourceId, message: Locator, index: number, name: string): Promise<void> {
  const dest = await save({ defaultPath: safeFileName(name), title: "Save attachment" });
  if (!dest) return;
  try {
    await api.saveAttachment(source, message, index, dest);
    store.toast(`Saved ${name}`);
  } catch (e) {
    store.toast(`Could not save ${name}: ${errMsg(e)}`, "error");
  }
}

/** Opens `r` with the OS default app; attached messages open in the reading pane instead. */
export async function openAttachmentRow(r: AttachmentRow): Promise<void> {
  const name = fileName(r.filename);
  if (opensAsMessage(r)) {
    store.showCarryingMessage(r);
    store.openEmbedded(r.source, r.message, r.index, name);
    return;
  }
  await openAttachment(r.source, r.message, r.index, name);
}

export function saveAttachmentRow(r: AttachmentRow): Promise<void> {
  return saveAttachmentAs(r.source, r.message, r.index, fileName(r.filename));
}

/** Toast for a "save several" result. */
export function reportSaved(r: SaveAllReport): void {
  const n = r.written.length;
  const saved = `Saved ${n} attachment${n === 1 ? "" : "s"}`;
  if (r.failed.length === 0) store.toast(saved);
  else store.toast(`${saved}, ${r.failed.length} failed — ${fileName(r.failed[0][0])}: ${r.failed[0][1]}`, "error");
}

/** Saves `rows` into a picked folder; the backend picks unique names, never overwriting files. */
export async function saveAttachmentRows(rows: AttachmentRow[]): Promise<void> {
  if (rows.length === 0) return;
  const dir = await open({ directory: true, title: `Save ${rows.length} attachments to…` });
  if (!dir) return;
  try {
    reportSaved(
      await api.saveAttachmentList(
        rows.map(({ source, message, index }) => ({ source, message, index })),
        dir,
      ),
    );
  } catch (e) {
    store.toast(`Could not save attachments: ${errMsg(e)}`, "error");
  }
}
