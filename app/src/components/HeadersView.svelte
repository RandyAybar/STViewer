<script module lang="ts">
  /** Kept across messages: whether the full header list is open, and raw vs table. */
  let showAll = $state(false);
  let showRaw = $state(false);
</script>

<script lang="ts">
  import Icon from "./Icon.svelte";
  import { Copier } from "../lib/clipboard.svelte";
  import { fullDateTime, hopTime, longDate } from "../lib/format";
  import { authChecks, duration, first, groupOf, hops, parseHeaders, spam, type HeaderGroup } from "../lib/headers";
  import type { Address, Message } from "../lib/types";

  let { msg }: { msg: Message } = $props();

  const h = $derived(msg.header);
  const raw = $derived(msg.raw_headers?.trim() || null);
  const headers = $derived(raw ? parseHeaders(raw) : []);
  const path = $derived(hops(headers));
  const checks = $derived(authChecks(headers));
  const spamV = $derived(spam(headers));
  const transit = $derived.by(() => {
    const t = path.map((p) => p.date).filter((d): d is number => d != null);
    return t.length > 1 ? Math.max(...t) - Math.min(...t) : null;
  });

  let filter = $state("");
  let showRefs = $state(false);
  let expanded = $state<Set<number>>(new Set());
  /** Recipient lists opened past their first entries. */
  let openLists = $state<Set<string>>(new Set());
  const copier = new Copier();

  // Per-message disclosure state starts closed for each email.
  $effect(() => {
    void msg;
    showRefs = false;
    expanded = new Set();
    openLists = new Set();
  });

  /** Raw header value with surrounding `<>`/`[]` removed. */
  function bare(v: string | null): string | null {
    const s = v?.trim().replace(/^[<[]+|[>\]]+$/g, "");
    return s || null;
  }

  /** `Name <a@b>` or a bare address as an [`Address`]; `null` for anything else. */
  function asAddr(v: string): Address | null {
    const m = /^\s*"?([^"<]*?)"?\s*<([^<>\s]+@[^<>\s]+)>\s*$/.exec(v);
    if (m) return { name: m[1].trim() || null, email: m[2] };
    return /^[^\s@<>]+@[^\s@<>]+$/.test(v.trim()) ? { name: null, email: v.trim() } : null;
  }

  const extras = $derived.by(() => {
    const rows: { label: string; value: string; addr: Address | null }[] = [];
    const add = (label: string, v: string | null | undefined, isAddr = false) =>
      v && rows.push({ label, value: v, addr: isAddr ? asAddr(v) : null });
    const fromEmail = h.from?.email?.toLowerCase();
    const sender = first(headers, "Sender");
    if (sender && fromEmail && !sender.toLowerCase().includes(fromEmail)) add("Sender", sender, true);
    const replyTo = first(headers, "Reply-To");
    if (replyTo && !(fromEmail && replyTo.toLowerCase().includes(fromEmail))) add("Reply to", replyTo, true);
    const rp = bare(first(headers, "Return-Path"));
    if (rp && rp.toLowerCase() !== fromEmail) add("Bounces to", rp, true);
    add("Sent with", first(headers, "X-Mailer") ?? first(headers, "User-Agent"));
    add("Sender IP", bare(first(headers, "X-Originating-IP")));
    const imp = first(headers, "Importance") ?? first(headers, "X-Priority");
    if (imp && !/^(normal|3)\b/i.test(imp)) add("Importance", imp.charAt(0).toUpperCase() + imp.slice(1));
    const sens = first(headers, "Sensitivity");
    if (sens && !/^normal/i.test(sens)) add("Sensitivity", sens);
    if (h.message_class && h.message_class !== "IPM.Note") add("Item type", h.message_class);
    return rows;
  });

  const recipients = $derived(
    (
      [
        ["To", h.to],
        ["Cc", h.cc],
        ["Bcc", h.bcc],
      ] as [string, Address[]][]
    ).filter(([, l]) => l.length),
  );

  const GROUPS: [HeaderGroup, string][] = [
    ["message", "Message"],
    ["delivery", "Delivery & authentication"],
    ["other", "Technical"],
  ];
  const visible = $derived.by(() => {
    const q = filter.trim().toLowerCase();
    return headers
      .map((x, i) => ({ ...x, i }))
      .filter((x) => !q || x.name.toLowerCase().includes(q) || x.value.toLowerCase().includes(q));
  });

  function toggleRow(i: number) {
    const s = new Set(expanded);
    if (s.has(i)) s.delete(i);
    else s.add(i);
    expanded = s;
  }
  const LONG = 180;
</script>

{#snippet addr(a: Address)}
  <span class="addr">
    {#if a.name?.trim() && a.name.trim() !== a.email}
      <span class="name">{a.name.trim()}</span>
      {#if a.email && /^\/o=/i.test(a.email)}
        <span class="email" title={a.email}>internal Exchange address</span>
      {:else if a.email}
        <span class="email">{a.email}</span>
      {/if}
    {:else}
      <span class="name">{a.email ?? ""}</span>
    {/if}
  </span>
{/snippet}

{#snippet id(label: string, value: string)}
  <dt>{label}</dt>
  <dd class="mono-val">
    <span>{value}</span>
    <button class="copy icon-btn" title="Copy" aria-label="Copy {label}" onclick={() => copier.copy(label, value)}>
      <Icon name="copy" size={12} />
    </button>
    {#if copier.copied === label}<span class="copied" role="status">Copied</span>{/if}
  </dd>
{/snippet}

<div class="hv selectable">
  <section>
    <h2>Overview</h2>
    <dl class="kv">
      <dt>From</dt>
      <dd>
        {#if h.from}{@render addr(h.from)}{:else}<span class="muted">(unknown sender)</span>{/if}
      </dd>
      {#each recipients as [label, list] (label)}
        <dt>{label}</dt>
        <dd class="addrs">
          {#each openLists.has(label) ? list : list.slice(0, 6) as a, i (i)}{@render addr(a)}{/each}
          {#if list.length > 6 && !openLists.has(label)}
            <button class="link" onclick={() => (openLists = new Set([...openLists, label]))}
              >+{list.length - 6} more</button
            >
          {/if}
        </dd>
      {/each}
      <dt>Date</dt>
      <dd>
        <span title={fullDateTime(h.date)}>{longDate(h.date) || "—"}</span>
        {#if first(headers, "Date")}
          {@const sent = first(headers, "Date") ?? ""}
          <!-- Verbatim header: the sender's own clock and offset; the tooltip converts it. -->
          <div class="sub" title={Number.isNaN(Date.parse(sent)) ? "" : fullDateTime(Date.parse(sent))}>
            Sender's clock: {sent}
          </div>
        {/if}
      </dd>
      <dt>Subject</dt>
      <dd>{h.subject || "(no subject)"}</dd>
      {#each extras as x (x.label)}
        <dt>{x.label}</dt>
        <dd>
          {#if x.addr}{@render addr(x.addr)}{:else}{x.value}{/if}
        </dd>
      {/each}
      {#if h.message_id}{@render id("Message ID", h.message_id)}{/if}
      {#if h.in_reply_to}{@render id("In reply to", h.in_reply_to)}{/if}
      {#if h.references.length}
        <dt>References</dt>
        <dd>
          <button class="link" onclick={() => (showRefs = !showRefs)}>
            {h.references.length} earlier {h.references.length === 1 ? "message" : "messages"}
          </button>
          {#if showRefs}
            <ol class="refs">
              {#each h.references as r, i (i)}<li>{r}</li>{/each}
            </ol>
          {/if}
        </dd>
      {/if}
    </dl>
  </section>

  {#if !raw}
    <section>
      <p class="note">
        No internet headers were stored for this email. This is normal for mail sent inside the same Exchange
        organisation, drafts, and items created in Outlook.
      </p>
    </section>
  {:else}
    {#if checks.length || spamV}
      <section>
        <h2>Security</h2>
        <div class="checks">
          {#each checks as c (c.method)}
            <span class="check {c.verdict}" title="{c.method}: {c.result}">
              <span class="dot"></span><b>{c.method}</b>{c.result}
            </span>
          {/each}
          {#if spamV}
            <span class="check {spamV.verdict}"><span class="dot"></span><b>Spam</b>{spamV.label}</span>
          {/if}
        </div>
      </section>
    {/if}

    {#if path.length}
      <section>
        <h2>
          Delivery path
          <span class="h-note">
            {path.length}
            {path.length === 1 ? "server" : "servers"}{transit != null ? ` · ${duration(transit)} in transit` : ""}
          </span>
        </h2>
        <ol class="path">
          {#each path as p, i (i)}
            {@const prev = i > 0 ? path[i - 1].date : null}
            <li>
              <span class="node"></span>
              <div class="hop">
                <div class="hop-top">
                  <span class="host" title={p.by ?? ""}>{p.by ?? "unknown server"}</span>
                  {#if p.protocol}<span class="pill">{p.protocol}</span>{/if}
                  {#if p.tls}<span class="pill secure"><Icon name="lock" size={10} /> {p.tls}</span>{/if}
                  <span class="grow"></span>
                  {#if prev != null && p.date != null && p.date - prev >= 1000}
                    <span class="delay" class:slow={p.date - prev > 5 * 60_000}>+{duration(p.date - prev)}</span>
                  {/if}
                  <span class="when" title={fullDateTime(p.date)}>{hopTime(p.date)}</span>
                </div>
                {#if p.from}
                  <div class="hop-from" title={p.fromDetail ?? ""}>
                    received from {p.from}{#if p.fromDetail && p.fromDetail !== p.from}<span class="ip"
                        >&ensp;·&ensp;{p.fromDetail}</span
                      >{/if}
                  </div>
                {/if}
              </div>
            </li>
          {/each}
        </ol>
      </section>
    {/if}

    <section>
      <h2>
        <button class="disclosure" aria-expanded={showAll} onclick={() => (showAll = !showAll)}>
          <Icon name={showAll ? "chevronDown" : "chevronRight"} size={13} />
          All headers <span class="h-note">{headers.length}</span>
        </button>
      </h2>
      {#if showAll}
        <div class="tools">
          {#if !showRaw}
            <input type="search" placeholder="Filter headers" bind:value={filter} />
          {:else}
            <span class="grow"></span>
          {/if}
          <div class="seg" role="group">
            <button class:on={!showRaw} onclick={() => (showRaw = false)}>Table</button>
            <button class:on={showRaw} onclick={() => (showRaw = true)}>Raw</button>
          </div>
          <button class="btn" onclick={() => copier.copy("__all", raw ?? "")}>
            <Icon name="copy" size={13} />
            {copier.copied === "__all" ? "Copied" : "Copy all"}
          </button>
        </div>
        {#if showRaw}
          <pre class="raw">{raw}</pre>
        {:else}
          {#each GROUPS as [g, title] (g)}
            {@const rows = visible.filter((x) => groupOf(x.name) === g)}
            {#if rows.length}
              <h3>{title} <span class="h-note">{rows.length}</span></h3>
              <div class="table">
                {#each rows as r (r.i)}
                  {@const long = r.value.length > LONG}
                  <div class="row">
                    <div class="hname" title={r.name}>{r.name}</div>
                    <div class="hval" class:clamp={long && !expanded.has(r.i)}>
                      {r.value || "—"}
                    </div>
                    <div class="row-act">
                      {#if long}
                        <button class="link" onclick={() => toggleRow(r.i)}
                          >{expanded.has(r.i) ? "less" : "more"}</button
                        >
                      {/if}
                    </div>
                  </div>
                {/each}
              </div>
            {/if}
          {/each}
          {#if !visible.length}<p class="note">No header matches “{filter}”.</p>{/if}
        {/if}
      {/if}
    </section>
  {/if}
</div>

<style>
  .hv {
    padding: 6px 20px 24px;
    max-width: 980px;
    container-type: inline-size;
  }
  section {
    padding: 14px 0;
    border-bottom: 1px solid var(--border);
  }
  section:last-child {
    border-bottom: none;
  }
  h2 {
    margin: 0 0 10px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-3);
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  h3 {
    margin: 14px 0 4px;
    font-size: 12px;
    font-weight: 600;
    color: var(--fg-2);
  }
  .h-note {
    font-weight: 400;
    letter-spacing: 0;
    text-transform: none;
    color: var(--fg-3);
  }
  .kv {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    gap: 7px 14px;
    margin: 0;
  }
  dt {
    color: var(--fg-3);
    font-size: 12px;
    text-align: right;
    padding-top: 1px;
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .sub {
    color: var(--fg-3);
    font-size: 12px;
    margin-top: 1px;
  }
  .addrs {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 6px;
    align-items: center;
  }
  .addr {
    display: inline-flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0 5px;
    max-width: 100%;
  }
  .addrs .addr {
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--bg-3);
  }
  .addr .name {
    overflow-wrap: anywhere;
  }
  .addr .email {
    color: var(--fg-3);
    font-size: 12px;
    overflow-wrap: anywhere;
  }
  .mono-val {
    display: flex;
    align-items: center;
    gap: 4px;
    font: 12px/1.5 var(--mono);
    color: var(--fg-2);
  }
  .mono-val span:first-child {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .copy {
    width: 20px;
    height: 20px;
    opacity: 0;
  }
  .mono-val:hover .copy,
  .copy:focus-visible {
    opacity: 1;
  }
  .copied {
    font: 11px var(--font);
    color: var(--accent);
  }
  .refs {
    margin: 6px 0 0;
    padding-left: 22px;
    font: 12px/1.6 var(--mono);
    color: var(--fg-2);
  }
  .link {
    font-size: 12px;
  }
  .note {
    margin: 0;
    color: var(--fg-2);
    font-size: 12px;
  }

  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .check {
    --c: var(--fg-3);
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 3px 10px 3px 8px;
    border-radius: 999px;
    border: 1px solid color-mix(in srgb, var(--c) 35%, var(--border));
    background: color-mix(in srgb, var(--c) 9%, transparent);
    font-size: 12px;
    color: var(--fg-2);
  }
  .check b {
    color: var(--fg);
    font-weight: 600;
  }
  .check .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--c);
  }
  .check.pass {
    --c: #22a05a;
  }
  .check.fail {
    --c: var(--danger);
  }
  .check.warn {
    --c: var(--warn);
  }

  .path {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .path li {
    position: relative;
    display: flex;
    gap: 12px;
    padding-bottom: 12px;
  }
  .path li:last-child {
    padding-bottom: 0;
  }
  .path li:not(:last-child)::before {
    content: "";
    position: absolute;
    left: 4px;
    top: 12px;
    bottom: -2px;
    width: 2px;
    background: color-mix(in srgb, var(--accent) 35%, var(--border));
    z-index: 0;
  }
  .node {
    position: relative;
    z-index: 1;
    box-sizing: border-box;
    flex: none;
    width: 10px;
    height: 10px;
    margin-top: 4px;
    border-radius: 50%;
    border: 2px solid var(--accent);
    background: var(--bg);
  }
  .path li:last-child .node {
    background: var(--accent);
  }
  .hop {
    flex: 1;
    min-width: 0;
  }
  .hop-top {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .host {
    font-weight: 500;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .pill {
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0 6px;
    border-radius: 4px;
    background: var(--bg-3);
    color: var(--fg-2);
    font-size: 11px;
    line-height: 17px;
    white-space: nowrap;
  }
  .pill.secure {
    color: #22a05a;
    background: color-mix(in srgb, #22a05a 12%, transparent);
  }
  .grow {
    flex: 1;
  }
  .delay {
    flex: none;
    font-size: 11px;
    color: var(--fg-3);
  }
  .delay.slow {
    color: var(--warn);
    font-weight: 600;
  }
  .when {
    flex: none;
    font-size: 12px;
    color: var(--fg-2);
    font-variant-numeric: tabular-nums;
  }
  .hop-from {
    margin-top: 1px;
    font-size: 12px;
    color: var(--fg-3);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .disclosure {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    border: none;
    background: none;
    padding: 0;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
  }
  .disclosure:hover {
    color: var(--fg);
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 4px;
  }
  .tools input {
    flex: 1;
    max-width: 320px;
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
  }
  .seg button {
    border: none;
    background: var(--bg);
    padding: 3px 10px;
    color: var(--fg-2);
  }
  .seg button.on {
    background: var(--bg-3);
    color: var(--fg);
  }
  .table {
    border: 1px solid var(--border);
    border-radius: 8px;
    overflow: hidden;
  }
  .row {
    display: grid;
    grid-template-columns: 180px minmax(0, 1fr) auto;
    gap: 12px;
    padding: 5px 10px;
    font-size: 12px;
    line-height: 1.5;
  }
  .row:nth-child(even) {
    background: var(--bg-2);
  }
  .hname {
    color: var(--fg-2);
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hval {
    font-family: var(--mono);
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
  .hval.clamp {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .row-act {
    min-width: 30px;
    text-align: right;
  }
  /* Narrow reading pane: header name above its value; server name on its own line. */
  @container (max-width: 640px) {
    .hop-top {
      flex-wrap: wrap;
      row-gap: 2px;
    }
    .host {
      flex-basis: 100%;
    }
    .row {
      grid-template-columns: minmax(0, 1fr) auto;
      gap: 0 12px;
    }
    .hval {
      grid-column: 1 / -1;
      grid-row: 2;
    }
  }
  .raw {
    margin: 4px 0 0;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: var(--bg-2);
    font: 12px/1.5 var(--mono);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
</style>
