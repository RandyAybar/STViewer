// Display labels for calendar, contact and task fields (`ItemDetails`).
import { formatWhen } from "./format";
import type { BusyStatus, MeetingKind, TaskStatus } from "./types";

export const BUSY_LABEL: Record<BusyStatus, string> = {
  free: "Free",
  tentative: "Tentative",
  busy: "Busy",
  out_of_office: "Out of office",
  working_elsewhere: "Working elsewhere",
};

/** Badge on meeting messages (the calendar entry itself has none). */
export const MEETING_LABEL: Record<MeetingKind, string> = {
  request: "Meeting request",
  cancellation: "Cancelled",
  accepted: "Accepted",
  tentative: "Tentative",
  declined: "Declined",
  counter: "Proposed new time",
};

export const TASK_STATUS_LABEL: Record<TaskStatus, string> = {
  not_started: "Not started",
  in_progress: "In progress",
  complete: "Completed",
  waiting: "Waiting on someone else",
  deferred: "Deferred",
};

/** `business` → `Business` (labels arrive lowercase). */
export function labelText(label: string): string {
  return label.charAt(0).toUpperCase() + label.slice(1);
}

const FREQ: Record<string, [unit: string, adverb: string]> = {
  DAILY: ["day", "Daily"],
  WEEKLY: ["week", "Weekly"],
  MONTHLY: ["month", "Monthly"],
  YEARLY: ["year", "Yearly"],
};
const WEEKDAY: Record<string, string> = { MO: "Mon", TU: "Tue", WE: "Wed", TH: "Thu", FR: "Fri", SA: "Sat", SU: "Sun" };

/**
 * Recurrence as stored, in words when it is a simple iCalendar `RRULE` (`FREQ=WEEKLY;BYDAY=MO,WE`
 * → `Weekly on Mon, Wed`). Outlook's own pattern text and rules with parts not covered here
 * (`BYMONTHDAY`, `BYSETPOS`, `1MO`, …) are returned unchanged.
 */
export function recurrenceText(rule: string): string {
  const parts = new Map<string, string>();
  for (const kv of rule.replace(/^RRULE:/i, "").split(";")) {
    const [k, v = ""] = kv.split("=", 2);
    if (k.trim()) parts.set(k.trim().toUpperCase(), v.trim().toUpperCase());
  }
  const freq = FREQ[parts.get("FREQ") ?? ""];
  const days = parts.get("BYDAY")?.split(",") ?? [];
  const interval = Number(parts.get("INTERVAL") ?? 1);
  const count = parts.get("COUNT");
  const until = /^(\d{4})(\d{2})(\d{2})/.exec(parts.get("UNTIL") ?? "");
  const known = [...parts.keys()].every((k) => ["FREQ", "INTERVAL", "BYDAY", "COUNT", "UNTIL", "WKST"].includes(k));
  const valid =
    freq &&
    known &&
    Number.isInteger(interval) &&
    interval >= 1 &&
    days.every((d) => WEEKDAY[d]) &&
    (count == null || /^\d+$/.test(count)) &&
    (until != null || !parts.has("UNTIL"));
  if (!valid) return rule;
  let text = interval > 1 ? `Every ${interval} ${freq[0]}s` : freq[1];
  if (days.length) text += ` on ${days.map((d) => WEEKDAY[d]).join(", ")}`;
  if (count) text += `, ${count} times`;
  if (until) text += `, until ${formatWhen({ kind: "date", value: `${until[1]}-${until[2]}-${until[3]}` })}`;
  return text;
}
