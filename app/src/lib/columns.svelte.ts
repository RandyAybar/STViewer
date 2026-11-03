/** One grid column of a list (header cell + the matching cell of every row). */
export interface ColumnDef {
  id: string;
  /** Default `grid-template-columns` track, e.g. `92px` or `minmax(80px, 1fr)`. */
  track: string;
  /** Narrowest width a drag may produce, px. */
  min: number;
  /** Absorbs the remaining width; its handle resizes the next column instead. */
  flex?: boolean;
  /** Not resizable (icon columns). */
  fixed?: boolean;
}

export interface HeaderCol extends ColumnDef {
  label: string;
  title?: string;
  align?: "right" | "center";
  /** Highlighted as the sort column. */
  active?: boolean;
  /** Sort direction when this is the sort column (arrow + `aria-sort`). */
  direction?: "ascending" | "descending";
  onsort?: () => void;
}

/**
 * User-chosen column widths of one list type, persisted in localStorage. Lists apply
 * `template()` as a CSS variable on their container so header and all virtual rows
 * re-layout together while dragging.
 */
export class ColumnWidths {
  readonly #key: string;
  widths = $state<Record<string, number>>({});

  constructor(key: string) {
    this.#key = key;
    try {
      const v: unknown = JSON.parse(localStorage.getItem(key) ?? "null");
      if (v && typeof v === "object") this.widths = v as Record<string, number>;
    } catch {
      // Corrupt entry: defaults.
    }
  }

  template(cols: ColumnDef[]): string {
    return cols
      .map((c) => {
        const w = this.widths[c.id];
        return w != null && !c.flex && !c.fixed ? `${w}px` : c.track;
      })
      .join(" ");
  }

  /** Live update while dragging; call `save()` when the drag ends. */
  set(id: string, px: number): void {
    this.widths[id] = px;
  }

  save(): void {
    localStorage.setItem(this.#key, JSON.stringify(this.widths));
  }

  clear(id: string): void {
    delete this.widths[id];
    this.save();
  }

  reset(): void {
    this.widths = {};
    localStorage.removeItem(this.#key);
  }
}

/** Column layout of the message list: item folders get columns for their item fields. */
export type ListKind = "mail" | "calendar" | "contacts" | "tasks";

/**
 * Widths per layout (columns differ). Folder, All mail and search results share the mail
 * set, which keeps its original key.
 */
export const messageColumns: Record<ListKind, ColumnWidths> = {
  mail: new ColumnWidths("stv.cols.messages"),
  calendar: new ColumnWidths("stv.cols.calendar"),
  contacts: new ColumnWidths("stv.cols.contacts"),
  tasks: new ColumnWidths("stv.cols.tasks"),
};
export const attachmentColumns = new ColumnWidths("stv.cols.attachments");
export const taggedColumns = new ColumnWidths("stv.cols.tagged");
