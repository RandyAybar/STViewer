// Generates mbox fixtures for exercising the app UI.
//
//   node scripts/make-fixtures.mjs            → testdata/app/demo.mbox (small, committed)
//   node scripts/make-fixtures.mjs --large N  → testdata/large/bulk-N.mbox (N messages, gitignored)
//
// Deterministic (seeded PRNG) so screenshots are reproducible.
import { mkdirSync, readFileSync, writeFileSync, createWriteStream } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");

let seed = 0x5eed;
const rand = () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
const pick = (a) => a[Math.floor(rand() * a.length)];

const PEOPLE = [
  ["Alice Johnson", "alice.johnson@northwind.example"],
  ["Bob Martinez", "bob@contoso.example"],
  ["Carol Nguyen", "carol.nguyen@fabrikam.example"],
  ["David Okafor", "d.okafor@northwind.example"],
  ["Eva Schmidt", "eva.schmidt@tailspin.example"],
  ["Frank Li", "frank.li@contoso.example"],
  ["Grace Park", "grace@adatum.example"],
  ["Hiro Tanaka", "hiro.tanaka@fabrikam.example"],
];
const ME = ["Jordan Example", "jordan@stviewer.example"];
const TOPICS = [
  ["Quarterly budget review", "the Q3 budget numbers look tight; marketing is 12% over plan and travel is under."],
  ["Contract renewal for Fabrikam", "legal sent back the redlines on the renewal. Section 7 (liability cap) still needs a decision."],
  ["Offsite planning", "we have three venue options for the spring offsite. The lakeside one has the best rooms but no projector."],
  ["Server migration weekend", "the database cutover is scheduled for Saturday 02:00 UTC. Rollback plan is attached."],
  ["Hiring: senior backend engineer", "two strong candidates made it to the final round. Feedback forms are due Friday."],
  ["Invoice 4471 overdue", "accounts receivable flagged invoice 4471 as 45 days overdue. Can you chase the customer?"],
  ["Design review: search UI", "the facet sidebar tested well; users wanted year filters and sender filters most."],
  ["Lunch on Thursday?", "there is a new ramen place near the office. Thursday at noon work for you?"],
  ["Security incident follow-up", "the phishing campaign targeted finance. Two people clicked; passwords have been reset."],
  ["Holiday schedule", "office closes Dec 24 through Jan 1. On-call rotation is in the shared calendar."],
];
const LABELS = ["Important", "Work", "Finance", "Travel", "Personal"];

const PNG_B64 = readFileSync(join(root, "app", "src-tauri", "icons", "32x32.png")).toString("base64");

function fmtDate(ms) {
  return new Date(ms).toUTCString().replace("GMT", "+0000");
}
function fromLineDate(ms) {
  const d = new Date(ms);
  const days = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
  const mons = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  const p = (n) => String(n).padStart(2, "0");
  return `${days[d.getUTCDay()]} ${mons[d.getUTCMonth()]} ${p(d.getUTCDate())} ${p(d.getUTCHours())}:${p(d.getUTCMinutes())}:${p(d.getUTCSeconds())} ${d.getUTCFullYear()}`;
}
function wrap64(s) {
  return s.replace(/.{1,76}/g, "$&\n").trimEnd();
}
function escapeFrom(body) {
  return body.replace(/^(>*From )/gm, ">$1");
}

// Realistic delivery headers for an inbound message: four `Received:` hops (newest first, a
// few seconds apart, with TLS versions), SPF/DKIM/DMARC results and common vendor headers.
// Only reserved names (`*.example`) and documentation addresses (RFC 5737 / RFC 3849);
// derived from the message number so the PRNG sequence (and the rest of the file) is unchanged.
function transportHeaders(n, from, date) {
  const addr = from[1];
  const domain = addr.split("@")[1];
  const t = (s) => fmtDate(date + s * 1000);
  const client = `192.0.2.${10 + (n % 200)}`;
  const relay = `198.51.100.${20 + (n % 30)}`;
  const out = `198.51.100.${60 + (n % 30)}`;
  const id = (k) => `${(date + k).toString(36).toUpperCase()}${n}`;
  return [
    `Return-Path: <${addr}>`,
    `Received: from mx1.stviewer.example ([2001:db8:25::10]) by mailbox.stviewer.example with LMTP id ${id(4)} for <${ME[1]}>; ${t(6)}`,
    `Received: from mail-out.${domain} (mail-out.${domain} [${out}]) by mx1.stviewer.example (Postfix) with ESMTPS id ${id(3)} (version=TLS1_3 cipher=TLS_AES_256_GCM_SHA384 bits=256) for <${ME[1]}>; ${t(4)}`,
    `Received: from smtp.${domain} (smtp.${domain} [${relay}]) by mail-out.${domain} with ESMTPS id ${id(2)} (version=TLS1_2 cipher=ECDHE-RSA-AES256-GCM-SHA384 bits=256); ${t(2)}`,
    `Received: from [${client}] (laptop.${domain} [${client}]) by smtp.${domain} with ESMTPSA id ${id(1)} (version=TLS1_3 cipher=TLS_AES_128_GCM_SHA256 bits=128); ${t(0)}`,
    `Authentication-Results: mx1.stviewer.example; spf=pass (domain of ${addr} designates ${out} as permitted sender) smtp.mailfrom=${addr}; dkim=pass header.d=${domain} header.s=s2025 header.b=Zk3q9w; dmarc=pass (p=REJECT) header.from=${domain}`,
    `Received-SPF: pass (mx1.stviewer.example: domain of ${addr} designates ${out} as permitted sender) client-ip=${out}; envelope-from=${addr};`,
    `X-MS-Exchange-Organization-SCL: 1`,
    `X-Mailer: ${n % 2 ? "Microsoft Outlook 16.0" : "Apple Mail (2.3774.600.62)"}`,
  ];
}

// `transport`: message number seeding `transportHeaders`, or falsy for none.
function message({ id, from, to, cc, date, subject, inReplyTo, refs, text, html, attachments, read, labels, transport }) {
  const h = [
    ...(transport ? transportHeaders(transport, from, date) : []),
    `From: ${from[0]} <${from[1]}>`,
    `To: ${to.map((p) => `${p[0]} <${p[1]}>`).join(", ")}`,
  ];
  if (cc?.length) h.push(`Cc: ${cc.map((p) => `${p[0]} <${p[1]}>`).join(", ")}`);
  h.push(`Subject: ${subject}`, `Date: ${fmtDate(date)}`, `Message-ID: <${id}>`);
  if (inReplyTo) h.push(`In-Reply-To: <${inReplyTo}>`);
  if (refs?.length) h.push(`References: ${refs.map((r) => `<${r}>`).join(" ")}`);
  if (labels?.length) h.push(`X-GM-LABELS: ${labels.join(",")}`);
  h.push(`Status: ${read ? "RO" : "O"}`, "MIME-Version: 1.0");

  const B = `=_b_${id.replace(/\W/g, "")}`;
  let body;
  if (!html && !attachments?.length) {
    h.push("Content-Type: text/plain; charset=utf-8", "Content-Transfer-Encoding: 8bit");
    body = text;
  } else {
    const alt = `${B}_alt`;
    const altPart = [
      `Content-Type: multipart/alternative; boundary="${alt}"`,
      "",
      `--${alt}`,
      "Content-Type: text/plain; charset=utf-8",
      "",
      text,
      `--${alt}`,
      "Content-Type: text/html; charset=utf-8",
      "",
      html ?? `<p>${text}</p>`,
      `--${alt}--`,
    ].join("\n");
    const inline = attachments?.filter((a) => a.cid) ?? [];
    const files = attachments?.filter((a) => !a.cid) ?? [];
    let core = altPart;
    if (inline.length) {
      const rel = `${B}_rel`;
      core = [
        `Content-Type: multipart/related; boundary="${rel}"`,
        "",
        `--${rel}`,
        altPart,
        ...inline.flatMap((a) => [
          `--${rel}`,
          `Content-Type: ${a.type}; name="${a.name}"`,
          "Content-Transfer-Encoding: base64",
          `Content-ID: <${a.cid}>`,
          `Content-Disposition: inline; filename="${a.name}"`,
          "",
          wrap64(a.b64),
        ]),
        `--${rel}--`,
      ].join("\n");
    }
    if (files.length) {
      h.push(`Content-Type: multipart/mixed; boundary="${B}"`);
      body = [
        "This is a multi-part message in MIME format.",
        `--${B}`,
        core,
        ...files.flatMap((a) =>
          a.raw
            ? [`--${B}`, `Content-Type: message/rfc822; name="${a.name}"`, `Content-Disposition: attachment; filename="${a.name}"`, "", a.raw]
            : [
                `--${B}`,
                `Content-Type: ${a.type}; name="${a.name}"`,
                "Content-Transfer-Encoding: base64",
                `Content-Disposition: attachment; filename="${a.name}"`,
                "",
                wrap64(a.b64),
              ],
        ),
        `--${B}--`,
      ].join("\n");
    } else {
      const [ct, ...rest] = core.split("\n");
      h.push(ct);
      body = rest.slice(1).join("\n");
    }
  }
  return `From ${from[1]} ${fromLineDate(date)}\n${h.join("\n")}\n\n${escapeFrom(body)}\n\n`;
}

function demo() {
  const out = [];
  const start = Date.UTC(2021, 0, 4, 9, 0, 0);
  const end = Date.UTC(2026, 9, 20, 18, 0, 0);
  let n = 0;
  const threads = [];
  for (let i = 0; i < 64; i++) {
    const date = start + Math.floor(((end - start) * i) / 64 + rand() * 86400000 * 3);
    const replyTo = i > 4 && rand() < 0.45 ? pick(threads) : null;
    const [topic, line] = replyTo ? [replyTo.subject.replace(/^Re: /, ""), replyTo.line] : pick(TOPICS);
    const from = rand() < 0.2 ? ME : pick(PEOPLE);
    const to = from === ME ? [pick(PEOPLE)] : [ME];
    const cc = rand() < 0.35 ? [pick(PEOPLE), pick(PEOPLE)].filter((p) => p !== from) : [];
    const id = `msg${++n}.${date}@stviewer.example`;
    const subject = replyTo ? `Re: ${topic}` : topic;
    const text = `Hi ${to[0][0].split(" ")[0]},\n\n${replyTo ? "Following up on this — " : ""}${line}\n\nSee https://intranet.northwind.example/wiki/${topic.toLowerCase().replace(/\W+/g, "-")} for details.\n\n${replyTo ? "> " + replyTo.line + "\n\n" : ""}Thanks,\n${from[0].split(" ")[0]}`;
    const attachments = [];
    let html = null;
    if (i % 5 === 0) {
      html = `<div style="font-family:Segoe UI,Arial,sans-serif"><table style="border-bottom:3px solid #2563eb;width:100%"><tr><td><img src="cid:logo-${n}@stviewer" width="32" height="32" alt="logo"></td><td style="font-size:18px;font-weight:600">${topic}</td></tr></table><p>Hi ${to[0][0].split(" ")[0]},</p><p>${line}</p><p style="color:#555">Tracking pixel (remote, should be blocked): <img src="https://tracker.example/pixel.gif?id=${n}" width="1" height="1"></p><ul><li>Owner: <b>${from[0]}</b></li><li>Due: ${new Date(date + 7 * 86400000).toDateString()}</li></ul><p><a href="https://intranet.northwind.example/wiki">Open the wiki</a></p></div>`;
      attachments.push({ cid: `logo-${n}@stviewer`, name: "logo.png", type: "image/png", b64: PNG_B64 });
    }
    if (i % 4 === 1) {
      const csv = `item,amount\nmarketing,${Math.round(rand() * 50000)}\ntravel,${Math.round(rand() * 9000)}\nnotes,"reconciliation pending for invoice 4471"\n`;
      attachments.push({ name: `budget-${2021 + Math.floor(i / 13)}.csv`, type: "text/csv", b64: Buffer.from(csv).toString("base64") });
    }
    if (i % 7 === 3) {
      attachments.push({ name: "photo.png", type: "image/png", b64: PNG_B64 });
      attachments.push({ name: "minutes.txt", type: "text/plain", b64: Buffer.from(`Minutes: ${topic}\nDecision: approved the rollback plan.\nAction items: ${from[0]} to circulate the redlines.\n`).toString("base64") });
    }
    const labels = rand() < 0.5 ? [pick(LABELS)] : [];
    out.push(
      message({
        id,
        from,
        to,
        cc,
        date,
        subject,
        inReplyTo: replyTo?.id,
        refs: replyTo ? [...replyTo.refs, replyTo.id] : [],
        text,
        html,
        attachments,
        read: rand() < 0.7,
        labels,
        // Inbound mail carries delivery headers (seeded by the message number).
        transport: from === ME ? 0 : n,
      }),
    );
    const rec = { id, subject, line, refs: replyTo ? [...replyTo.refs, replyTo.id] : [] };
    threads.push(rec);
  }
  // One message whose body contains an mbox-hostile "From " line.
  out.push(
    message({
      id: `fromline@stviewer.example`,
      from: PEOPLE[0],
      to: [ME],
      date: Date.UTC(2026, 9, 28, 8, 30),
      subject: "Escaping check: From lines in bodies",
      text: "This body has a line that starts with From:\nFrom the desk of Alice — this must not split the message.\nEnd.",
      read: false,
    }),
  );
  // A forward carrying the original as message/rfc822 (nested view), itself with an attachment.
  const original = message({
    id: "original-contract@fabrikam.example",
    from: PEOPLE[2],
    to: [PEOPLE[1]],
    date: Date.UTC(2026, 9, 25, 14, 5),
    subject: "=?UTF-8?Q?Signed_contract_=E2=80=94_Fabrikam_renewal?=",
    text: "Bob,\n\nAttached is the countersigned renewal. Liability cap stays at 2x annual fees.\n\nCarol",
    html: "<p>Bob,</p><p>Attached is the <b>countersigned renewal</b>. Liability cap stays at 2x annual fees.</p><p>Carol</p>",
    attachments: [{ name: "renewal-terms.txt", type: "text/plain", b64: Buffer.from("Term: 24 months\nLiability cap: 2x annual fees\n").toString("base64") }],
    read: true,
  }).replace(/^From [^\n]*\n/, "").trimEnd();
  out.push(
    message({
      id: "fwd-contract@contoso.example",
      from: PEOPLE[1],
      to: [ME],
      date: Date.UTC(2026, 9, 27, 9, 12),
      subject: "Fwd: =?UTF-8?Q?Signed_contract_=E2=80=94_Fabrikam_renewal?=",
      text: "FYI — original message attached.\n\nBob",
      attachments: [{ name: "Signed contract.eml", type: "message/rfc822", raw: original }],
      read: false,
      labels: ["Important"],
    }),
  );
  const dir = join(root, "testdata", "app");
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "demo.mbox"), out.join(""));
  console.log(`wrote ${join(dir, "demo.mbox")} (${out.length} messages)`);
}

function large(count) {
  const dir = join(root, "testdata", "large");
  mkdirSync(dir, { recursive: true });
  const path = join(dir, `bulk-${count}.mbox`);
  const ws = createWriteStream(path);
  const start = Date.UTC(2010, 0, 1);
  let buf = "";
  for (let i = 0; i < count; i++) {
    const from = pick(PEOPLE);
    const [topic, line] = pick(TOPICS);
    const date = start + i * 600000 + Math.floor(rand() * 300000);
    buf += message({
      id: `bulk${i}@stviewer.example`,
      from,
      to: [ME],
      date,
      subject: `${topic} #${i}`,
      text: `${line}\nReference number ${i}.`,
      read: rand() < 0.8,
    });
    if (buf.length > 1 << 20) {
      ws.write(buf);
      buf = "";
    }
  }
  ws.end(buf);
  ws.on("finish", () => console.log(`wrote ${path} (${count} messages)`));
}

const li = process.argv.indexOf("--large");
if (li > 0) large(Number(process.argv[li + 1] ?? 500000));
else demo();
