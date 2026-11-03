/** Copies `text`; falls back to a temporary selection where the Clipboard API is unavailable. */
async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.append(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}

/** Copy buttons with "Copied" feedback: `copied` is the id of the last copy for 1.5 s. */
export class Copier {
  copied = $state<string | null>(null);
  #timer: ReturnType<typeof setTimeout> | undefined;

  async copy(id: string, text: string): Promise<void> {
    await copyText(text);
    clearTimeout(this.#timer);
    this.copied = id;
    this.#timer = setTimeout(() => (this.copied = null), 1500);
  }
}
