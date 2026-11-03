import { open } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { SvelteSet } from "svelte/reactivity";
import { api, errMsg } from "./api";
import type { ListKind } from "./columns.svelte";
import { IS_MAC } from "./format";
import {
  AllMailList,
  AttachmentList,
  FolderList,
  SearchList,
  TaggedList,
  type AttachmentFilter,
  type PagedList,
  type SearchScope,
} from "./lists.svelte";
import type {
  AgentAccess,
  AttachmentCategory,
  AttachmentRow,
  AttachmentSort,
  AttachmentSortKey,
  Folder,
  FolderId,
  IndexProgress,
  Locator,
  Row,
  Sort,
  SortKey,
  SourceId,
  SourceInfo,
  Tag,
  TagTarget,
} from "./types";

export interface ReaderEntry {
  source: SourceId;
  locator: Locator;
  /** Label shown in the breadcrumb for nested (embedded) messages. */
  label: string;
}

export interface Toast {
  id: number;
  text: string;
  kind: "info" | "error";
}

export type SearchScopeKind = "all" | "source" | "folder";

function folderKey(source: SourceId, folder: FolderId): string {
  return `${source}\u0000${folder}`;
}

export type AttView = "details" | "icons";

/** Attachment-view settings shared by all sources; persisted in localStorage. */
export interface AttPrefs {
  view: AttView;
  sort: AttachmentSort;
  includeInline: boolean;
  excludeCategories: AttachmentCategory[];
  /** Lowercase, no dot. */
  excludeExtensions: string[];
  /** Bytes. */
  minSize: number | null;
  maxSize: number | null;
}

const ATT_PREFS_KEY = "stv.attachments";
const ATT_DEFAULTS: AttPrefs = {
  view: "details",
  sort: { key: "date", descending: true },
  includeInline: false,
  excludeCategories: [],
  excludeExtensions: [],
  minSize: null,
  maxSize: null,
};

function loadAttPrefs(): AttPrefs {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(ATT_PREFS_KEY) ?? "null");
    if (v && typeof v === "object") return { ...ATT_DEFAULTS, ...(v as Partial<AttPrefs>) };
  } catch {
    // Corrupt entry: fall back to defaults.
  }
  return { ...ATT_DEFAULTS };
}

/** Refresh interval for counts that follow a source that is still indexing. */
const PROGRESS_REFRESH_MS = 3000;
class Store {
  sources = $state<SourceInfo[]>([]);
  progress = $state<Record<number, IndexProgress>>({});
  folders = $state<Record<number, Folder[]>>({});
  folderErrors = $state<Record<number, string>>({});
  expandedSources = $state<Record<number, boolean>>({});
  collapsed = $state<Record<string, boolean>>({});
  current = $state.raw<{ source: SourceId; folder: FolderId } | null>(null);
  /** Folder sort per list layout; calendar and task folders open by start / due date. */
  folderSorts = $state.raw<Record<ListKind, Sort>>({
    mail: { key: "date", descending: true },
    calendar: { key: "start", descending: true },
    contacts: { key: "subject", descending: false },
    tasks: { key: "start", descending: true },
  });

  /** Folder listing (`$state.raw`: the instance carries its own reactive fields). */
  folderList = $state.raw<FolderList | null>(null);
  search = $state.raw<SearchList | null>(null);
  query = $state("");
  searchScope = $state<SearchScopeKind>("all");
  /** `null` = relevance. */
  searchSort = $state.raw<Sort | null>(null);
  /** "All mail" of one source (source root clicked); `null` = off. */
  allMail = $state.raw<AllMailList | null>(null);
  allMailSort = $state.raw<Sort>({ key: "date", descending: true });
  #allMailAt = 0;

  focus = $state(-1);
  anchor = -1;
  selection = new SvelteSet<number>();

  reader = $state.raw<ReaderEntry[]>([]);
  maximized = $state(false);

  toasts = $state<Toast[]>([]);
  exportOpen = $state(false);
  /** "Tagged" view of one source; `null` = off. */
  tagged = $state.raw<TaggedList | null>(null);
  /** Every tag (name, color, count across all sources). */
  tags = $state.raw<Tag[]>([]);
  /** Sidebar badge: tagged items per source. */
  taggedTotals = $state<Record<number, number>>({});
  manageTagsOpen = $state(false);
  settingsOpen = $state(false);
  /** MCP agent-access server state (`null` until loaded). */
  agent = $state.raw<AgentAccess | null>(null);
  /** Bumped on every agent tool call (status-bar flash). */
  agentPulse = $state(0);
  /** Attachments mode: the source whose attachments fill the middle pane (`null` = off). */
  attSource = $state<SourceId | null>(null);
  attachments = $state.raw<AttachmentList | null>(null);
  /** Raw filter-box text (debounced into the query). */
  attText = $state("");
  /** Category chips (include filter); empty = all. */
  attCategories = $state.raw<AttachmentCategory[]>([]);
  attPrefs = $state.raw<AttPrefs>(loadAttPrefs());
  /** Focused attachment shown in the preview pane. */
  attRow = $state.raw<AttachmentRow | null>(null);
  /** Sidebar badge: attachments per source under the current filter settings. */
  attTotals = $state<Record<number, number>>({});
  #attTimer: ReturnType<typeof setTimeout> | undefined;
  #attTotalAt: Record<number, number> = {};

  #toastId = 0;
  #started = false;

  /** Message list (search, all mail or folder); `null` in attachments and tagged views. */
  get list(): PagedList | null {
    return this.search ?? this.allMail ?? this.folderList;
  }

  /** Source the middle pane is showing (folder, all mail, attachments or tagged). */
  get contextSource(): SourceId | null {
    return this.current?.source ?? this.allMail?.source ?? this.attSource ?? this.tagged?.source ?? null;
  }

  /** The list keyboard navigation and selection operate on. */
  get nav(): PagedList | AttachmentList | TaggedList | null {
    return this.attachments ?? this.tagged ?? this.list;
  }

  sourceName(id: SourceId): string {
    return this.sources.find((s) => s.id === id)?.display_name ?? `Source ${id}`;
  }

  /** Mailbox message count: sum of folder counts once loaded, else the indexer's total. */
  sourceTotal(id: SourceId): number {
    const folders = this.folders[id];
    if (folders) return folders.reduce((n, f) => n + (f.message_count ?? 0), 0);
    const p = this.progress[id] ?? this.sources.find((s) => s.id === id)?.index;
    return p ? (p.total ?? p.indexed) : 0;
  }

  folderName(source: SourceId, folder: FolderId | null): string {
    if (folder == null) return "";
    const folders = this.folders[source];
    // Rows of a source whose tree is not loaded (search hits, attachments): fetch it once; the id shows meanwhile.
    if (!folders && !this.folderErrors[source]) void this.loadFolders(source);
    return folders?.find((f) => f.id === folder)?.name ?? folder;
  }

  toast(text: string, kind: Toast["kind"] = "info"): void {
    const id = ++this.#toastId;
    this.toasts.push({ id, text, kind });
    setTimeout(() => this.dismiss(id), kind === "error" ? 8000 : 4000);
  }

  dismiss(id: number): void {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  async init(): Promise<void> {
    if (this.#started) return;
    this.#started = true;
    void this.refreshTags();
    // Subscribe before reading the state the events update, so no change falls in between.
    await Promise.all([
      listen<IndexProgress[]>("index-progress", (e) => this.#applyProgress(e.payload)),
      listen<AgentAccess>("agent-access-state", (e) => (this.agent = e.payload)),
      // One event per agent tool call; only the status-bar flash uses it.
      listen("agent-access", () => this.agentPulse++),
    ]);
    void this.refreshAgent();
    // Read together; the progress snapshot still applies after the sources that seed it.
    const [, progress] = await Promise.allSettled([this.refreshSources(), api.indexProgress()]);
    if (progress.status === "fulfilled") this.#applyProgress(progress.value);
    else this.toast(`Could not read indexing progress: ${errMsg(progress.reason)}`, "error");
    const first = this.sources[0];
    if (first) await this.expandSource(first.id, true);
  }

  #applyProgress(list: IndexProgress[]): void {
    const prev = this.progress;
    const next: Record<number, IndexProgress> = {};
    for (const p of list) if (p.source != null) next[p.source] = p;
    this.progress = next;
    const now = Date.now();
    for (const p of Object.values(next)) {
      const id = p.source!;
      const finished = p.done && prev[id] != null && !prev[id].done;
      const due = (at: number) => finished || (!p.done && now - at > PROGRESS_REFRESH_MS);
      if (this.allMail?.source === id && due(this.#allMailAt)) {
        this.#allMailAt = now;
        void this.allMail.refreshCount();
      }
      if (!this.expandedSources[id] && this.attSource !== id) continue;
      if (due(this.#attTotalAt[id] ?? 0)) void this.refreshAttTotal(id, finished);
    }
  }

  async refreshSources(): Promise<void> {
    try {
      this.sources = await api.sources();
      for (const s of this.sources) if (!this.progress[s.id]) this.progress[s.id] = s.index;
    } catch (e) {
      this.toast(`Could not list sources: ${errMsg(e)}`, "error");
    }
  }

  async addPaths(paths: string[]): Promise<void> {
    if (paths.length === 0) return;
    try {
      const res = await api.addPaths(paths);
      for (const [path, err] of res.errors) this.toast(`${path}: ${err}`, "error");
      // Opening the first new source does not need the refreshed list; only the toast waits for it.
      const opened = res.added.length ? this.expandSource(res.added[0].id, true) : undefined;
      await this.refreshSources();
      if (res.added.length) {
        this.toast(`Added ${res.added.length} source${res.added.length > 1 ? "s" : ""}; indexing in background`);
        await opened;
      }
    } catch (e) {
      this.toast(errMsg(e), "error");
    }
  }

  /** "Add files…": picks mail archives and adds them. */
  async addFilesDialog(): Promise<void> {
    const sel = await open({
      multiple: true,
      title: "Add mail archives",
      // The macOS panel merges all filters into one allow-list (no file-type menu), so
      // extensionless mbox files would only be selectable through the catch-all entry.
      filters: IS_MAC
        ? []
        : [
            { name: "Mail archives", extensions: ["pst", "ost", "mbox", "mbx", "eml", "emlx", "msg", "dat", "tnef"] },
            { name: "All files (mbox without extension)", extensions: ["*"] },
          ],
    });
    if (sel) await this.addPaths(sel);
  }

  /** "Add folder…": a Thunderbird profile, Apple Mail export or directory of .eml/.msg files. */
  async addFolderDialog(): Promise<void> {
    const sel = await open({
      directory: true,
      multiple: true,
      title: "Add a mail folder (Thunderbird profile, .eml/.msg directory, Apple Mail)",
    });
    if (sel) await this.addPaths(sel);
  }

  async removeSource(id: SourceId): Promise<void> {
    try {
      await api.removeSource(id);
    } catch (e) {
      this.toast(errMsg(e), "error");
      return;
    }
    // Drop every view, row and badge that still points into the removed source.
    let dropped = false;
    if (this.current?.source === id) {
      this.current = null;
      this.folderList = null;
      dropped = true;
    }
    if (this.allMail?.source === id) {
      this.allMail = null;
      dropped = true;
    }
    if (this.attSource === id) {
      this.#leaveAttachments();
      dropped = true;
    }
    if (this.tagged?.source === id) {
      this.#leaveTagged();
      dropped = true;
    }
    const s = this.search;
    if (s) {
      const rest = s.scope.sources.filter((x) => x !== id);
      if (s.scope.sources.length > 0 && rest.length === 0) this.clearSearch();
      else this.search = new SearchList(s.query, { ...s.scope, sources: rest }, this.searchSort);
      dropped = true;
    }
    if (dropped) this.#resetSelection();
    if (this.reader.some((r) => r.source === id)) this.reader = [];
    delete this.attTotals[id];
    delete this.taggedTotals[id];
    delete this.folders[id];
    delete this.folderErrors[id];
    delete this.expandedSources[id];
    delete this.progress[id];
    void this.refreshAgent();
    await this.refreshSources();
  }

  async reindex(id: SourceId): Promise<void> {
    try {
      await api.reindex(id);
      this.toast(`Re-indexing ${this.sourceName(id)}`);
    } catch (e) {
      this.toast(errMsg(e), "error");
    }
  }

  /** In-flight folder-tree fetches, shared by concurrent callers. */
  #folderLoads = new Map<SourceId, Promise<Folder[]>>();

  loadFolders(id: SourceId): Promise<Folder[]> {
    const have = this.folders[id];
    if (have) return Promise.resolve(have);
    let p = this.#folderLoads.get(id);
    if (!p) {
      p = api
        .folders(id)
        .then(
          (f) => {
            this.folders[id] = f;
            delete this.folderErrors[id];
            return f;
          },
          (e: unknown) => {
            this.folderErrors[id] = errMsg(e);
            return [];
          },
        )
        .finally(() => this.#folderLoads.delete(id));
      this.#folderLoads.set(id, p);
    }
    return p;
  }

  /** Expands a source in the tree; with `select`, opens its first non-empty folder. */
  async expandSource(id: SourceId, select = false): Promise<void> {
    this.expandedSources[id] = true;
    void this.refreshAttTotal(id);
    void this.refreshTaggedTotal(id);
    const folders = await this.loadFolders(id);
    if (select && folders.length) {
      const target = folders.find((f) => (f.message_count ?? 0) > 0) ?? folders[0];
      this.selectFolder(id, target.id);
    }
  }

  toggleSource(id: SourceId): void {
    if (this.expandedSources[id]) this.expandedSources[id] = false;
    else void this.expandSource(id);
  }

  isCollapsed(source: SourceId, folder: FolderId): boolean {
    return this.collapsed[folderKey(source, folder)] ?? false;
  }

  toggleFolder(source: SourceId, folder: FolderId): void {
    const k = folderKey(source, folder);
    this.collapsed[k] = !(this.collapsed[k] ?? false);
  }

  selectFolder(source: SourceId, folder: FolderId): void {
    this.current = { source, folder };
    if (this.search) this.clearSearch(false);
    this.allMail = null;
    this.#leaveAttachments();
    this.#leaveTagged();
    this.#reloadFolder();
  }

  /** Lists every indexed message of `source` (source root clicked). */
  selectAllMail(source: SourceId): void {
    if (this.allMail?.source === source && !this.search) return;
    this.current = null;
    this.folderList = null;
    this.clearSearch();
    this.#leaveAttachments();
    this.#leaveTagged();
    this.allMail = new AllMailList(source, this.allMailSort);
    this.#allMailAt = Date.now();
    this.#resetSelection();
    // Expanding also loads the folder names shown in the Folder column.
    if (!this.expandedSources[source]) void this.expandSource(source);
  }

  #reloadFolder(): void {
    const cur = this.current;
    this.folderList = cur ? new FolderList(cur.source, cur.folder, this.folderSorts[this.listKind]) : null;
    this.#resetSelection();
  }

  #resetSelection(): void {
    this.focus = -1;
    this.anchor = -1;
    this.selection.clear();
  }

  setSort(key: SortKey): void {
    // A new key starts newest / largest first.
    const sort = (cur: Sort | null): Sort =>
      cur?.key === key
        ? { key, descending: !cur.descending }
        : { key, descending: key === "date" || key === "size" || key === "start" };
    if (this.search) {
      this.searchSort = sort(this.searchSort);
      this.#rerunSearch();
      return;
    }
    if (this.allMail) {
      this.allMailSort = sort(this.allMailSort);
      this.allMail = new AllMailList(this.allMail.source, this.allMailSort);
      this.#resetSelection();
      return;
    }
    const kind = this.listKind;
    this.folderSorts = { ...this.folderSorts, [kind]: sort(this.folderSorts[kind]) };
    this.#reloadFolder();
  }

  sortByRelevance(): void {
    this.searchSort = null;
    this.#rerunSearch();
  }

  get activeSort(): Sort | null {
    return this.search ? this.searchSort : this.allMail ? this.allMailSort : this.folderSorts[this.listKind];
  }

  /** Message-list layout: the open folder's item kind; search results and All mail list as mail. */
  get listKind(): ListKind {
    const cur = this.current;
    if (this.search || this.allMail || !cur) return "mail";
    const kind = this.folders[cur.source]?.find((f) => f.id === cur.folder)?.kind;
    return kind === "calendar" || kind === "contacts" || kind === "tasks" ? kind : "mail";
  }

  #scope(): SearchScope {
    const cur = this.current;
    const src = this.contextSource;
    if (this.searchScope === "source" && src != null) return { sources: [src], folder: null };
    if (this.searchScope === "folder" && cur) return { sources: [cur.source], folder: cur.folder };
    return { sources: [], folder: null };
  }

  runSearch(query = this.query, sources?: SourceId[]): void {
    const q = query.trim();
    this.query = query;
    if (!q) {
      this.clearSearch();
      return;
    }
    const scope = this.#scope();
    if (sources) scope.sources = sources;
    this.allMail = null;
    this.#leaveAttachments();
    this.#leaveTagged();
    this.search = new SearchList(q, scope, this.searchSort);
    this.#resetSelection();
  }

  #rerunSearch(): void {
    const s = this.search;
    if (!s) return;
    this.search = new SearchList(s.query, s.scope, this.searchSort);
    this.#resetSelection();
  }

  /** Narrows the running search to one source (facet click). */
  restrictToSource(id: SourceId): void {
    const s = this.search;
    if (!s) return;
    this.search = new SearchList(s.query, { sources: [id], folder: null }, this.searchSort);
    this.#resetSelection();
  }

  clearSearch(resetQuery = true): void {
    if (resetQuery) this.query = "";
    if (!this.search) return;
    this.search = null;
    this.#resetSelection();
  }

  // ---- attachments mode ----

  /** Shows `source`'s attachments in the middle pane (leaves folder / search). */
  selectAttachments(source: SourceId): void {
    if (this.attSource === source && this.attachments) {
      this.reader = [];
      return;
    }
    this.current = null;
    this.folderList = null;
    this.allMail = null;
    this.#leaveTagged();
    this.clearSearch();
    this.attSource = source;
    this.attText = "";
    this.attCategories = [];
    this.reader = [];
    this.#reloadAttachments();
  }

  #leaveAttachments(): void {
    if (this.attSource == null) return;
    clearTimeout(this.#attTimer);
    this.attSource = null;
    this.attachments = null;
    this.attRow = null;
  }

  #attFilter(source: SourceId, text: string, categories: AttachmentCategory[]): AttachmentFilter {
    const p = this.attPrefs;
    return {
      sources: [source],
      text,
      categories,
      exclude_categories: p.excludeCategories,
      exclude_extensions: p.excludeExtensions,
      min_size: p.minSize,
      max_size: p.maxSize,
      include_inline: p.includeInline,
      sort: p.sort,
    };
  }

  #reloadAttachments(): void {
    clearTimeout(this.#attTimer);
    const src = this.attSource;
    if (src == null) return;
    const prev = this.attachments;
    const filter = this.#attFilter(src, this.attText.trim(), this.attCategories);
    const keep = prev?.filter.sources[0] === src && !prev.error;
    this.attachments = new AttachmentList(filter, keep ? prev.categoryCounts : []);
    this.attRow = null;
    this.#resetSelection();
  }

  /** Re-runs the current attachment query (toolbar refresh). */
  refreshAttachments(): void {
    this.#reloadAttachments();
    if (this.attSource != null) void this.refreshAttTotal(this.attSource);
  }

  /** Filter-box input; re-queries after a short pause. */
  setAttText(text: string): void {
    this.attText = text;
    clearTimeout(this.#attTimer);
    this.#attTimer = setTimeout(() => {
      if (this.attachments?.filter.text !== this.attText.trim()) this.#reloadAttachments();
    }, 200);
  }

  /** Category chip click; `null` = "All". */
  toggleAttCategory(c: AttachmentCategory | null): void {
    if (c == null) this.attCategories = [];
    else if (this.attCategories.includes(c)) this.attCategories = this.attCategories.filter((x) => x !== c);
    else this.attCategories = [...this.attCategories, c];
    this.#reloadAttachments();
  }

  /** Updates persisted settings; re-queries unless only the view mode changed. */
  setAttPrefs(patch: Partial<AttPrefs>): void {
    const next = { ...this.attPrefs, ...patch };
    this.attPrefs = next;
    localStorage.setItem(ATT_PREFS_KEY, JSON.stringify(next));
    if (Object.keys(patch).every((k) => k === "view")) return;
    this.#reloadAttachments();
    if (Object.keys(patch).some((k) => k !== "view" && k !== "sort")) {
      for (const id of Object.keys(this.attTotals)) void this.refreshAttTotal(Number(id));
    }
  }

  /** Column-header click: same key flips direction; new key starts newest/largest first. */
  setAttSort(key: AttachmentSortKey): void {
    const cur = this.attPrefs.sort;
    this.setAttPrefs({
      sort:
        cur.key === key ? { key, descending: !cur.descending } : { key, descending: key === "date" || key === "size" },
    });
  }

  hideExtension(ext: string): void {
    const e = ext.toLowerCase().replace(/^\./, "");
    if (!e || this.attPrefs.excludeExtensions.includes(e)) return;
    this.setAttPrefs({ excludeExtensions: [...this.attPrefs.excludeExtensions, e] });
  }

  hideCategory(c: AttachmentCategory): void {
    if (this.attPrefs.excludeCategories.includes(c)) return;
    this.attCategories = this.attCategories.filter((x) => x !== c);
    this.setAttPrefs({ excludeCategories: [...this.attPrefs.excludeCategories, c] });
  }

  /** Number of active settings in the Filters panel (badge). */
  get attFilterCount(): number {
    const p = this.attPrefs;
    return (
      p.excludeCategories.length +
      p.excludeExtensions.length +
      (p.minSize != null ? 1 : 0) +
      (p.maxSize != null ? 1 : 0) +
      (p.includeInline ? 1 : 0)
    );
  }

  /** Resets the Filters panel and the category chips. */
  clearAttFilters(): void {
    this.attCategories = [];
    this.setAttPrefs({
      includeInline: false,
      excludeCategories: [],
      excludeExtensions: [],
      minSize: null,
      maxSize: null,
    });
  }

  /**
   * Refreshes the sidebar badge for `id`. While the source is still indexing, an empty
   * attachments view for it re-queries so rows appear; `finished` also reloads an
   * untouched view so it reflects the complete index.
   */
  async refreshAttTotal(id: SourceId, finished = false): Promise<void> {
    this.#attTotalAt[id] = Date.now();
    try {
      const res = await api.attachments({ ...this.#attFilter(id, "", []), offset: 0, limit: 1 });
      this.attTotals[id] = res.total;
      const att = this.attachments;
      if (
        this.attSource === id &&
        att &&
        !att.loading &&
        res.total > 0 &&
        (att.count === 0 || (finished && this.focus < 0))
      ) {
        this.#reloadAttachments();
      }
    } catch {
      // Badge is best-effort; the view itself reports query errors.
    }
  }

  /** Shows `row` in the preview pane (drops a message opened via "Show message"). */
  showAttachment(row: AttachmentRow): void {
    this.attRow = row;
    this.reader = [];
  }

  /** Opens the message carrying `row` in the reading pane (Esc returns to the preview). */
  showCarryingMessage(row: AttachmentRow): void {
    this.reader = [{ source: row.source, locator: row.message, label: row.subject || "(no subject)" }];
  }

  /** Selected attachment rows (fetching pages as needed) in list order. */
  async selectedAttachments(): Promise<AttachmentRow[]> {
    const list = this.attachments;
    if (!list) return [];
    return list.rowsAsync([...this.selection].sort((a, b) => a - b));
  }

  // ---- tags ----

  async refreshTags(): Promise<void> {
    try {
      this.tags = await api.tags();
    } catch {
      // Keep the previous list; tag commands report their own errors.
    }
  }

  async refreshTaggedTotal(id: SourceId): Promise<void> {
    try {
      this.taggedTotals[id] = await api.taggedCount(id);
    } catch {
      // Badge is best-effort.
    }
  }

  tagColor(name: string): string {
    return this.tags.find((t) => t.name === name)?.color ?? "var(--fg-3)";
  }

  /** Lists the tagged emails and attachments of `source`, optionally only those with `tag`. */
  selectTagged(source: SourceId, tag: string | null = null): void {
    if (this.tagged?.source === source && this.tagged.tag === tag) {
      this.reader = [];
      return;
    }
    if (this.tagged?.source !== source) {
      this.current = null;
      this.folderList = null;
      this.allMail = null;
      this.clearSearch();
      this.#leaveAttachments();
      if (!this.expandedSources[source]) void this.expandSource(source);
    }
    this.tagged = new TaggedList(source, tag);
    this.attRow = null;
    this.reader = [];
    this.#resetSelection();
  }

  #leaveTagged(): void {
    if (!this.tagged) return;
    this.tagged = null;
    this.attRow = null;
  }

  /** Tag targets of the selection in whichever list is showing, in list order. */
  async selectedTargets(): Promise<TagTarget[]> {
    const idx = [...this.selection].sort((a, b) => a - b);
    const att = this.attachments;
    const tagged = this.tagged;
    const list = this.list;
    if (att) {
      const rows = await att.rowsAsync(idx);
      return rows.map((r) => ({ source: r.source, message: r.message, attachment: r.index }));
    }
    if (tagged) return (await tagged.rowsAsync(idx)).map((r) => r.target);
    if (!list) return [];
    const out: TagTarget[] = [];
    for (const r of await list.rowsAsync(idx)) {
      if (r.header.locator) out.push({ source: r.source, message: r.header.locator, attachment: null });
    }
    return out;
  }

  /** Tags or untags `targets`, then refreshes tag counts, badges and the Tagged view. */
  async setTag(targets: TagTarget[], tag: string, on: boolean): Promise<void> {
    if (targets.length === 0) return;
    try {
      await api.setTag(targets, tag, on);
    } catch (e) {
      this.toast(`Could not ${on ? "tag" : "untag"}: ${errMsg(e)}`, "error");
    }
    await this.#afterTagChange();
  }

  async renameTag(old: string, name: string): Promise<void> {
    try {
      await api.renameTag(old, name);
    } catch (e) {
      this.toast(`Could not rename ${old}: ${errMsg(e)}`, "error");
    }
    await this.#afterTagChange(this.tagged?.tag === old ? name : undefined);
  }

  async deleteTag(name: string): Promise<void> {
    try {
      await api.deleteTag(name);
    } catch (e) {
      this.toast(`Could not delete ${name}: ${errMsg(e)}`, "error");
    }
    await this.#afterTagChange(this.tagged?.tag === name ? null : undefined);
  }

  /** `filter` replaces the Tagged view's chip filter (renamed / deleted tag). */
  async #afterTagChange(filter?: string | null): Promise<void> {
    for (const id of Object.keys(this.expandedSources)) {
      if (this.expandedSources[Number(id)]) void this.refreshTaggedTotal(Number(id));
    }
    // The Tagged view reloads once the tag list is fresh, so its pills and chips get the (new / renamed) colors.
    await this.refreshTags();
    const t = this.tagged;
    if (t) {
      this.tagged = new TaggedList(t.source, filter === undefined ? t.tag : filter);
      this.attRow = null;
      this.#resetSelection();
    }
  }

  // ---- agent access (MCP) ----

  async refreshAgent(): Promise<void> {
    try {
      this.agent = await api.agentAccessGet();
    } catch {
      // Older backend or transient failure: the Settings dialog shows "Off".
    }
  }

  /** Applies the agent-access settings; on failure reports the error and reloads the real state. */
  async setAgent(enabled: boolean, port: number, allowed: SourceId[]): Promise<boolean> {
    try {
      this.agent = await api.agentAccessSet(enabled, port, allowed);
      return true;
    } catch (e) {
      this.toast(`Agent access: ${errMsg(e)}`, "error");
      await this.refreshAgent();
      return false;
    }
  }

  async regenerateAgentToken(): Promise<void> {
    try {
      this.agent = await api.agentAccessRegenerateToken();
    } catch (e) {
      this.toast(`Could not regenerate the token: ${errMsg(e)}`, "error");
    }
  }

  // ---- selection ----

  async setFocus(i: number, mode: "single" | "toggle" | "range" = "single"): Promise<void> {
    const list = this.nav;
    if (!list || list.count === 0) return;
    i = Math.max(0, Math.min(list.count - 1, i));
    this.focus = i;
    if (mode === "toggle") {
      if (this.selection.has(i)) this.selection.delete(i);
      else this.selection.add(i);
      this.anchor = i;
    } else if (mode === "range" && this.anchor >= 0) {
      this.selection.clear();
      const [a, b] = this.anchor < i ? [this.anchor, i] : [i, this.anchor];
      for (let k = a; k <= b; k++) this.selection.add(k);
    } else {
      this.selection.clear();
      this.selection.add(i);
      this.anchor = i;
    }
    if (list instanceof AttachmentList) {
      const row = await list.rowAsync(i);
      if (row && this.focus === i && this.attachments === list) this.showAttachment(row);
      return;
    }
    if (list instanceof TaggedList) {
      const item = await list.rowAsync(i);
      if (!item || this.focus !== i || this.tagged !== list) return;
      if (item.attachment) this.showAttachment(item.attachment);
      else {
        this.attRow = null;
        this.reader = [
          { source: item.target.source, locator: item.target.message, label: item.header.subject || "(no subject)" },
        ];
      }
      return;
    }
    const row = await list.rowAsync(i);
    if (row && this.focus === i && this.list === list) this.showRow(row);
  }

  /** Row/tile click or Space: Shift extends from the anchor, Ctrl/Cmd toggles, plain selects one. */
  clickItem(i: number, e: { shiftKey: boolean; ctrlKey: boolean; metaKey: boolean }): void {
    void this.setFocus(i, e.shiftKey ? "range" : e.ctrlKey || e.metaKey ? "toggle" : "single");
  }

  showRow(row: Row): void {
    const loc = row.header.locator;
    if (!loc) return;
    this.reader = [{ source: row.source, locator: loc, label: row.header.subject || "(no subject)" }];
  }

  openEmbedded(source: SourceId, parent: Locator, attachment: number, label: string): void {
    this.reader = [...this.reader, { source, locator: { t: "embedded", parent, attachment }, label }];
  }

  popReader(to: number): void {
    this.reader = this.reader.slice(0, to + 1);
  }

  /** Selected emails (fetching pages as needed) in list order; tagged attachments count as their email. */
  async selectedItems(): Promise<[SourceId, Locator][]> {
    if (this.tagged) {
      const seen = new Set<string>();
      const out: [SourceId, Locator][] = [];
      for (const t of await this.selectedTargets()) {
        const key = `${t.source}\u0000${JSON.stringify(t.message)}`;
        if (seen.has(key)) continue;
        seen.add(key);
        out.push([t.source, t.message]);
      }
      return out;
    }
    const list = this.list;
    if (!list) return [];
    const out: [SourceId, Locator][] = [];
    for (const r of await list.rowsAsync([...this.selection].sort((a, b) => a - b))) {
      if (r.header.locator) out.push([r.source, r.header.locator]);
    }
    return out;
  }
}

export const store = new Store();
