<script lang="ts">
  import Icon from "./Icon.svelte";
  import { canOpenExternal, mailtoHref, openExternal, webHref } from "../lib/external";
  import { addrFull, formatWhen, whenFull, whenRange } from "../lib/format";
  import { BUSY_LABEL, MEETING_LABEL, TASK_STATUS_LABEL, labelText, recurrenceText } from "../lib/items";
  import type { Address, ItemDetails, MeetingKind } from "../lib/types";

  /** Calendar, contact or task fields shown above the body of a non-mail item. */
  let {
    item,
    inline = false,
  }: {
    item: ItemDetails;
    /** Content-sized, no scroll cap (conversation cards). */
    inline?: boolean;
  } = $props();

  /** Attendees shown before "+N more". */
  const FOLD = 6;
  /** Attendee lists opened past their first entries. */
  let openLists = $state<Record<string, boolean>>({});

  const TONE: Partial<Record<MeetingKind, string>> = {
    request: "accent",
    accepted: "accent",
    cancellation: "danger",
    declined: "danger",
  };

  const contactName = $derived(
    item.type === "contact" ? item.display_name || [item.given_name, item.surname].filter(Boolean).join(" ") : "",
  );

  const webLink = $derived(item.type === "contact" && item.web_page ? webHref(item.web_page) : null);
</script>

{#snippet row(label: string, value: string | null, title?: string)}
  {#if value}
    <dt>{label}</dt>
    <dd class="text" {title}>{value}</dd>
  {/if}
{/snippet}

{#snippet people(label: string, list: Address[])}
  {#if list.length}
    {@const shown = openLists[label] ? list : list.slice(0, FOLD)}
    <dt>{label}</dt>
    <dd>
      {shown.map(addrFull).join("; ")}
      {#if list.length > shown.length}
        <button class="link" onclick={() => (openLists[label] = true)}>+{list.length - shown.length} more</button>
      {/if}
    </dd>
  {/if}
{/snippet}

<section class="item selectable" class:inline aria-label="Item details">
  {#if item.type === "appointment"}
    <div class="title">
      <Icon name="calendar" size={14} />
      <span class="kind">{item.meeting ? "Meeting" : "Appointment"}</span>
      {#if item.meeting}<span class="badge {TONE[item.meeting] ?? ''}">{MEETING_LABEL[item.meeting]}</span>{/if}
    </div>
    <dl class="meta">
      {#if item.start || item.end}
        <dt>{item.meeting === "counter" ? "Proposed" : "When"}</dt>
        <dd title={whenFull(item.start)}>
          {whenRange(item.start, item.end)}
          {#if item.all_day}<span class="badge">All day</span>{/if}
        </dd>
      {/if}
      {@render row("Where", item.location)}
      {@render row("Organizer", item.organizer && addrFull(item.organizer))}
      {@render people("Required", item.required)}
      {@render people("Optional", item.optional)}
      {@render people("Resources", item.resources)}
      {@render row("Show as", item.busy_status && BUSY_LABEL[item.busy_status])}
      {@render row(
        "Recurs",
        item.recurrence ? recurrenceText(item.recurrence) : item.recurring ? "Yes" : null,
        item.recurrence ?? undefined,
      )}
    </dl>
  {:else if item.type === "contact"}
    <div class="title">
      <Icon name="contacts" size={14} />
      <span class="name">{contactName || "Contact"}</span>
    </div>
    {#if item.job_title || item.company || item.department}
      <div class="sub">{[item.job_title, item.company, item.department].filter(Boolean).join(" · ")}</div>
    {/if}
    <dl class="meta">
      {#each item.emails as e, i (i)}
        <dt>{i === 0 ? "Email" : `Email ${i + 1}`}</dt>
        <dd>
          {#if e.email}
            <button class="link" title="Write to {e.email}" onclick={() => openExternal(mailtoHref(e.email ?? ""))}
              >{addrFull(e)}</button
            >
          {:else}{addrFull(e)}{/if}
        </dd>
      {/each}
      {#each item.phones as p, i (i)}{@render row(labelText(p.label), p.value)}{/each}
      {#each item.addresses as a, i (i)}{@render row(`${labelText(a.label)} address`, a.value)}{/each}
      {@render row("Birthday", formatWhen(item.birthday))}
      {#if item.web_page}
        <dt>Web page</dt>
        <dd>
          {#if webLink && canOpenExternal(webLink)}
            <button class="link" title="Open {webLink}" onclick={() => openExternal(webLink)}>{item.web_page}</button>
          {:else}{item.web_page}{/if}
        </dd>
      {/if}
    </dl>
  {:else}
    <div class="title">
      <Icon name="tasks" size={14} />
      <span class="kind">Task</span>
    </div>
    <dl class="meta">
      {@render row("Status", item.status && TASK_STATUS_LABEL[item.status])}
      {#if item.percent_complete != null}
        <dt>Complete</dt>
        <dd class="progress">
          <span
            class="bar"
            role="progressbar"
            aria-label="Percent complete"
            aria-valuenow={item.percent_complete}
            aria-valuemin={0}
            aria-valuemax={100}><span style:width="{Math.min(100, item.percent_complete)}%"></span></span
          >
          {item.percent_complete}%
        </dd>
      {/if}
      {@render row("Start", formatWhen(item.start), whenFull(item.start))}
      {@render row("Due", formatWhen(item.due), whenFull(item.due))}
      {@render row("Completed", formatWhen(item.completed), whenFull(item.completed))}
      {@render row("Owner", item.owner)}
    </dl>
  {/if}
</section>

<style>
  .item {
    flex: none;
    max-height: 40%;
    overflow: auto;
    padding: 8px 20px 10px;
    border-bottom: 1px solid var(--border);
  }
  .item.inline {
    max-height: none;
    padding: 6px 14px 8px;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
    color: var(--fg-2);
  }
  .kind {
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .name {
    color: var(--fg);
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .sub {
    margin: -4px 0 6px 20px;
    color: var(--fg-2);
    font-size: 12px;
  }
  .badge.accent {
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent);
  }
  .badge.danger {
    background: color-mix(in srgb, var(--danger) 14%, transparent);
    color: var(--danger);
  }
  .meta {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 10px;
    margin: 0;
    font-size: 12.5px;
  }
  dt {
    color: var(--fg-3);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  /** Postal addresses and recurrence text keep their line breaks. */
  .text {
    white-space: pre-line;
  }
  .link {
    text-align: left;
    overflow-wrap: anywhere;
  }
  .progress {
    display: flex;
    align-items: center;
    gap: 8px;
    font-variant-numeric: tabular-nums;
  }
  .bar {
    width: 120px;
    height: 6px;
    border-radius: 3px;
    background: var(--bg-3);
    overflow: hidden;
  }
  .bar > span {
    display: block;
    height: 100%;
    background: var(--accent);
  }
</style>
