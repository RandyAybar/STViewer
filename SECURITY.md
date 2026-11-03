# Security policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Report them privately
through GitHub's private vulnerability reporting: open the repository's **Security** tab
and choose **Report a vulnerability**. Please include the affected version, the steps to
reproduce, and the impact. Never include real mailbox content. Use a synthetic file that
reproduces the problem.

We aim to acknowledge reports within a week. Fixes go into the next release, and the
reporter is credited unless they ask not to be.

## Supported versions

Only the latest release gets security fixes.

## Threat model (summary)

STViewer reads untrusted input (email archives and the messages inside them) on the
user's own computer.

- **Malformed archives.** PST/OST, `.msg`, TNEF and MIME parsers must never crash, hang
  or read out of bounds on hostile input. They return errors for corrupt structures.
  Archives are opened read-only and are never modified.
- **HTML mail.** Message HTML is rendered in a sandboxed iframe with scripts disabled and
  a restrictive Content-Security-Policy. Remote content (tracking pixels, external
  images and styles) is blocked until the user allows it for that message.
- **Attachments.** Opening an attachment copies it to a fresh per-user folder in the
  app's cache directory, not a shared temp directory. The file is then handed to the OS
  default application, which carries the usual risks of the file type. On Windows,
  opened and saved attachments get the Mark of the Web (`Zone.Identifier`, Internet
  zone), so SmartScreen and Office Protected View apply. Programs and scripts (`.exe`,
  `.msi`, `.bat`, `.ps1`, `.js`, `.vbs`, `.lnk`, `.hta`, `.jar`, `.sh`, `.app`, `.pkg`, …)
  open only after a native warning prompt. Saved file names are sanitised against path
  traversal and reserved device names.
- **Agent access (MCP server).** It is off by default. When enabled:
  - it binds to `127.0.0.1` only;
  - it requires a 256-bit bearer token, compared in constant time;
  - it rejects any request with an `Origin` header (browsers) or a `Host` header other
    than `127.0.0.1:<port>` / `localhost:<port>` (DNS rebinding);
  - it exposes only the mailboxes the user selects, and only read-only;
  - it caps request bodies at 64 KiB and page offsets at 100 000;
  - it logs every call to `agent-access.log`, which rotates to `agent-access.log.1` at
    4 MB.

  The token is stored in plain text in `agent-access.json` in the user's library
  directory. On Unix the file is created with mode 0600. Local processes running as the
  same user are trusted: they can read that file, and the archives themselves. Mail
  content returned to an agent may contain prompt-injection text, so agents should treat
  it as untrusted data.

Out of scope: attacks that need local administrator access or control of the user
account; vulnerabilities in the OS WebView (WebView2, WKWebView, WebKitGTK) itself.
