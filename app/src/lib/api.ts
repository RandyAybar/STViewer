import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import type {
  AddResult,
  AgentAccess,
  AttachmentPage,
  AttachmentQuery,
  ConversationItem,
  ExportFormat,
  ExportReport,
  Folder,
  FolderId,
  Header,
  IndexProgress,
  Locator,
  Message,
  SaveAllReport,
  SearchOptions,
  SearchResults,
  Sort,
  SourceId,
  SourceInfo,
  Tag,
  TagTarget,
  TaggedItem,
} from "./types";

/** Tauri command wrappers; argument names match the Rust command parameters. */
export const api = {
  addPaths: (paths: string[]) => invoke<AddResult>("add_paths", { paths }),
  removeSource: (source: SourceId) => invoke<void>("remove_source", { source }),
  sources: () => invoke<SourceInfo[]>("sources"),
  folders: (source: SourceId) => invoke<Folder[]>("folders", { source }),
  count: (source: SourceId, folder: FolderId) => invoke<number>("count", { source, folder }),
  list: (source: SourceId, folder: FolderId, offset: number, limit: number, sort: Sort) =>
    invoke<Header[]>("list", { source, folder, offset, limit, sort }),
  message: (source: SourceId, locator: Locator) => invoke<Message>("message", { source, locator }),
  /** Extracts and opens with the OS default app; the temp path, or `null` when the user declined to run a program. */
  openAttachment: (source: SourceId, locator: Locator, index: number) =>
    invoke<string | null>("open_attachment", { source, locator, index }),
  saveAttachment: (source: SourceId, locator: Locator, index: number, dest: string) =>
    invoke<void>("save_attachment", { source, locator, index, dest }),
  /** Saves attachments of any messages into `dir` under unique names (existing files are kept). */
  saveAttachmentList: (items: { source: SourceId; message: Locator; index: number }[], dir: string) =>
    invoke<SaveAllReport>("save_attachment_list", { items, dir }),
  saveAttachments: (source: SourceId, locator: Locator, dir: string) =>
    invoke<SaveAllReport>("save_attachments", { source, locator, dir }),
  /** The conversation of a message: one entry per email (copies merged), oldest → newest. */
  conversation: (source: SourceId, locator: Locator) => invoke<ConversationItem[]>("conversation", { source, locator }),
  openFolder: (path: string) => invoke<void>("open_folder", { path }),
  search: (query: string, opts: SearchOptions) => invoke<SearchResults>("search", { query, opts }),
  indexProgress: () => invoke<IndexProgress[]>("index_progress"),
  reindex: (source: SourceId) => invoke<void>("reindex", { source }),
  /** All tags; `count` = tagged items across all sources. */
  tags: () => invoke<Tag[]>("tags"),
  /** Fails when the tag exists; `color` null = next palette color. */
  createTag: (name: string, color: string | null) => invoke<Tag>("create_tag", { name, color }),
  renameTag: (old: string, name: string) => invoke<void>("rename_tag", { old, new: name }),
  /** Removes the tag from every item. */
  deleteTag: (name: string) => invoke<void>("delete_tag", { name }),
  /** `on` tags (creating the tag if missing), otherwise untags. */
  setTag: (targets: TagTarget[], tag: string, on: boolean) => invoke<void>("set_tag", { targets, tag, on }),
  /** Current tags per target, same order. */
  tagsOf: (targets: TagTarget[]) => invoke<string[][]>("tags_of", { targets }),
  /** Newest-added first; `null` filters are "any". */
  tagged: (source: SourceId | null, tag: string | null) => invoke<TaggedItem[]>("tagged", { source, tag }),
  /** Items of `source` with at least one tag. */
  taggedCount: (source: SourceId) => invoke<number>("tagged_count", { source }),
  export: (items: [SourceId, Locator][], format: ExportFormat, dest: string) =>
    invoke<ExportReport>("export", { items, format, dest }),
  exportFolder: (source: SourceId, folder: FolderId, format: ExportFormat, dest: string) =>
    invoke<ExportReport>("export_folder", { source, folder, format, dest }),
  exportSearch: (query: string, opts: SearchOptions, format: ExportFormat, dest: string) =>
    invoke<ExportReport>("export_search", { query, opts, format, dest }),
  /** One page of a source's (or all sources') attachment listing. */
  attachments: (query: AttachmentQuery) => invoke<AttachmentPage>("attachments", { query }),
  /** First ~1500 characters of a document attachment's extracted text, or null. */
  attachmentText: (source: SourceId, locator: Locator, index: number) =>
    invoke<string | null>("attachment_text", { source, locator, index }),
  agentAccessGet: () => invoke<AgentAccess>("agent_access_get"),
  /** Starts, stops or restarts the MCP server; rejects (e.g. port in use) without changing state. */
  agentAccessSet: (enabled: boolean, port: number, allowedSources: SourceId[]) =>
    invoke<AgentAccess>("agent_access_set", { enabled, port, allowedSources }),
  /** New token; restarts a running server, so existing agent configs stop working. */
  agentAccessRegenerateToken: () => invoke<AgentAccess>("agent_access_regenerate_token"),
};

/** Human-readable message from a rejected command (`{kind, message}`) or any thrown value. */
export function errMsg(e: unknown): string {
  if (e && typeof e === "object" && "message" in e) return String(e.message);
  return String(e);
}

// ---- stv:// protocol URLs ----

/** `http://stv.localhost/` on Windows, `stv://localhost/` elsewhere. */
const STV_BASE = convertFileSrc("", "stv");

/** CSP source expression matching `STV_BASE`. */
export const STV_CSP_SOURCE = STV_BASE.startsWith("http") ? new URL(STV_BASE).origin : "stv:";

function b64url(s: string): string {
  const bytes = new TextEncoder().encode(s);
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function cidUrl(source: SourceId, locator: Locator, cid: string): string {
  return `${STV_BASE}cid/${source}/${b64url(JSON.stringify(locator))}/${encodeURIComponent(cid)}`;
}

export function attachmentUrl(source: SourceId, locator: Locator, index: number): string {
  return `${STV_BASE}att/${source}/${b64url(JSON.stringify(locator))}/${index}`;
}

/** JPEG picture (longest side ≤ 256 px) of an image or document attachment; 404 when nothing can draw it. */
export function thumbnailUrl(source: SourceId, locator: Locator, index: number): string {
  return `${STV_BASE}thumb/${source}/${b64url(JSON.stringify(locator))}/${index}`;
}

/** The same picture at up to 1024 px, for the preview pane. */
export function previewUrl(source: SourceId, locator: Locator, index: number): string {
  return `${STV_BASE}preview/${source}/${b64url(JSON.stringify(locator))}/${index}`;
}
