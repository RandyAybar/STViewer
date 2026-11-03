import { api, errMsg } from "./api";
import type {
  AttachmentCategory,
  AttachmentQuery,
  AttachmentRow,
  FacetCount,
  FolderId,
  Row,
  SearchOptions,
  Sort,
  SourceId,
  TaggedItem,
} from "./types";

const PAGE_SIZE = 200;
/** Pages kept in memory per list (LRU); 60 × 200 = 12k rows. */
const PAGE_CAP = 60;
/** Pages re-fetched by `invalidate` (a screenful plus prefetch). */
const REFETCH_ON_INVALIDATE = 3;

/**
 * Virtual-list data: a known row count plus pages of `PAGE_SIZE` rows fetched on
 * demand and kept in an LRU. `version` changes whenever a page lands so views re-read.
 */
export abstract class PagedList<T = Row> {
  count = $state(0);
  version = $state(0);
  loading = $state(true);
  error = $state<string | null>(null);
  /** Map iteration order doubles as LRU order (oldest first). */
  #pages = new Map<number, T[]>();
  #inflight = new Map<number, Promise<void>>();

  protected abstract fetchPage(offset: number, limit: number): Promise<T[]>;

  /** Row `i` if its page is resident; does not trigger a fetch. */
  row(i: number): T | undefined {
    const p = Math.floor(i / PAGE_SIZE);
    const page = this.#pages.get(p);
    if (!page) return undefined;
    // Touch for LRU.
    this.#pages.delete(p);
    this.#pages.set(p, page);
    return page[i - p * PAGE_SIZE];
  }

  /** Ensures pages covering `[start, end)` are resident or loading. */
  ensure(start: number, end: number): void {
    if (this.count === 0) return;
    const first = Math.max(0, Math.floor(start / PAGE_SIZE));
    const last = Math.floor((Math.min(end, this.count) - 1) / PAGE_SIZE);
    for (let p = first; p <= last; p++) void this.#load(p);
  }

  async rowAsync(i: number): Promise<T | undefined> {
    if (i < 0 || i >= this.count) return undefined;
    await this.#load(Math.floor(i / PAGE_SIZE));
    return this.row(i);
  }

  /** Loads page `p` unless resident; `refetch` replaces a resident page in place. */
  #load(p: number, refetch = false): Promise<void> {
    if (!refetch && this.#pages.has(p)) return Promise.resolve();
    const pending = this.#inflight.get(p);
    if (pending) return pending;
    const job = this.fetchPage(p * PAGE_SIZE, PAGE_SIZE)
      .then((rows) => {
        this.#pages.set(p, rows);
        while (this.#pages.size > PAGE_CAP) {
          const oldest = this.#pages.keys().next().value!;
          this.#pages.delete(oldest);
        }
        this.version++;
      })
      .catch((e: unknown) => {
        this.error = errMsg(e);
      })
      .finally(() => this.#inflight.delete(p));
    this.#inflight.set(p, job);
    return job;
  }

  protected seedPage(p: number, rows: T[]): void {
    this.#pages.set(p, rows);
    this.version++;
  }

  /**
   * The result set changed underneath (indexing added rows, which may sort anywhere):
   * re-fetches the most recently used pages — the visible ones — in place, so they stay
   * on screen until replaced, and forgets the rest.
   */
  protected invalidate(): void {
    const pages = [...this.#pages.keys()];
    for (const p of pages.slice(0, -REFETCH_ON_INVALIDATE)) this.#pages.delete(p);
    for (const p of pages.slice(-REFETCH_ON_INVALIDATE)) void this.#load(p, true);
    this.version++;
  }
}

/** Messages of one folder, in `sort` order. */
export class FolderList extends PagedList {
  readonly source: SourceId;
  readonly folder: FolderId;
  readonly sort: Sort;

  constructor(source: SourceId, folder: FolderId, sort: Sort) {
    super();
    this.source = source;
    this.folder = folder;
    this.sort = sort;
    api
      .count(source, folder)
      .then((n) => {
        this.count = n;
        this.ensure(0, PAGE_SIZE);
      })
      .catch((e: unknown) => {
        this.error = errMsg(e);
      })
      .finally(() => (this.loading = false));
  }

  protected async fetchPage(offset: number, limit: number): Promise<Row[]> {
    const headers = await api.list(this.source, this.folder, offset, limit, this.sort);
    return headers.map((header) => ({ source: this.source, header }));
  }
}

/**
 * Every indexed message of one source ("All mail"): the empty search query over that
 * source. `count` follows indexing via `refreshCount` without dropping loaded pages.
 */
export class AllMailList extends PagedList {
  readonly source: SourceId;
  readonly sort: Sort;

  constructor(source: SourceId, sort: Sort) {
    super();
    this.source = source;
    this.sort = sort;
    this.#first();
  }

  options(offset: number, limit: number): SearchOptions {
    return { offset, limit, sources: [this.source], folder: null, sort: this.sort, facets: false };
  }

  async #first(): Promise<void> {
    try {
      const res = await api.search("", this.options(0, PAGE_SIZE));
      this.count = res.total;
      this.seedPage(
        0,
        res.hits.map((h) => ({ source: h.source, header: h.header })),
      );
    } catch (e) {
      this.error = errMsg(e);
    } finally {
      this.loading = false;
    }
  }

  /** Picks up messages indexed since the list was created. */
  async refreshCount(): Promise<void> {
    if (this.loading) return;
    try {
      const res = await api.search("", this.options(0, 1));
      if (res.total === this.count) return;
      this.count = res.total;
      this.invalidate();
    } catch {
      // Keep the current count; the next progress tick retries.
    }
  }

  protected async fetchPage(offset: number, limit: number): Promise<Row[]> {
    const res = await api.search("", this.options(offset, limit));
    return res.hits.map((h) => ({ source: h.source, header: h.header }));
  }
}

export interface SearchScope {
  sources: SourceId[];
  folder: FolderId | null;
}

/** Full-text search results; the first page also carries totals and facets. */
export class SearchList extends PagedList {
  readonly query: string;
  readonly scope: SearchScope;
  readonly sort: Sort | null;
  facetsFrom = $state<FacetCount[]>([]);
  facetsYear = $state<FacetCount[]>([]);
  facetsSource = $state<FacetCount[]>([]);

  constructor(query: string, scope: SearchScope, sort: Sort | null) {
    super();
    this.query = query;
    this.scope = scope;
    this.sort = sort;
    this.#first();
  }

  options(offset: number, limit: number, facets: boolean): SearchOptions {
    return { offset, limit, sources: this.scope.sources, folder: this.scope.folder, sort: this.sort, facets };
  }

  async #first(): Promise<void> {
    try {
      const res = await api.search(this.query, this.options(0, PAGE_SIZE, true));
      this.count = res.total;
      this.facetsFrom = res.facets_from;
      this.facetsYear = res.facets_year;
      this.facetsSource = res.facets_source;
      this.seedPage(
        0,
        res.hits.map((h) => ({ source: h.source, header: h.header, snippet: h.snippet })),
      );
    } catch (e) {
      this.error = errMsg(e);
    } finally {
      this.loading = false;
    }
  }

  protected async fetchPage(offset: number, limit: number): Promise<Row[]> {
    const res = await api.search(this.query, this.options(offset, limit, false));
    return res.hits.map((h) => ({ source: h.source, header: h.header, snippet: h.snippet }));
  }
}

/** Attachment query without paging; the list fills in `offset`/`limit`. */
export type AttachmentFilter = Omit<AttachmentQuery, "offset" | "limit">;

/**
 * Tagged emails and attachments of one source. The backend returns the whole (small)
 * set at once; `all` keeps it unfiltered for the per-tag chip counts.
 */
export class TaggedList extends PagedList<TaggedItem> {
  readonly source: SourceId;
  /** Chip filter; `null` = every tagged item. */
  readonly tag: string | null;
  all = $state.raw<TaggedItem[]>([]);
  #rows: TaggedItem[] = [];

  constructor(source: SourceId, tag: string | null) {
    super();
    this.source = source;
    this.tag = tag;
    this.#first();
  }

  async #first(): Promise<void> {
    try {
      const all = await api.tagged(this.source, null);
      const tag = this.tag;
      this.#rows = tag == null ? all : all.filter((i) => i.tags.includes(tag));
      this.all = all;
      this.count = this.#rows.length;
      this.seedPage(0, this.#rows.slice(0, PAGE_SIZE));
    } catch (e) {
      this.error = errMsg(e);
    } finally {
      this.loading = false;
    }
  }

  protected async fetchPage(offset: number, limit: number): Promise<TaggedItem[]> {
    return this.#rows.slice(offset, offset + limit);
  }
}

/** Attachment listing; the first page also carries totals and per-category counts. */
export class AttachmentList extends PagedList<AttachmentRow> {
  readonly filter: AttachmentFilter;
  categoryCounts = $state.raw<[AttachmentCategory, number][]>([]);

  /** `counts` seeds `categoryCounts` so filter chips do not flicker while re-querying. */
  constructor(filter: AttachmentFilter, counts: [AttachmentCategory, number][] = []) {
    super();
    this.filter = filter;
    this.categoryCounts = counts;
    this.#first();
  }

  async #first(): Promise<void> {
    try {
      const res = await api.attachments({ ...this.filter, offset: 0, limit: PAGE_SIZE });
      this.count = res.total;
      this.categoryCounts = res.category_counts;
      this.seedPage(0, res.rows);
    } catch (e) {
      this.error = errMsg(e);
    } finally {
      this.loading = false;
    }
  }

  protected async fetchPage(offset: number, limit: number): Promise<AttachmentRow[]> {
    return (await api.attachments({ ...this.filter, offset, limit })).rows;
  }
}
