//! End to end over real HTTP with the official rmcp client: auth, sharing scope, tools.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::ServiceExt;
use serde_json::{json, Value};
use stv_library::Library;
use stv_mcp::{AccessLog, AccessLogEntry};

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn testdata(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(rel)
}

fn wait_indexed(lib: &Library) {
    let start = Instant::now();
    while lib.is_indexing() || lib.index_progress().iter().any(|p| !p.done) {
        assert!(start.elapsed() < Duration::from_secs(120), "indexing timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
}

async fn call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ClientConfig>,
    tool: &str,
    args: Value,
) -> (bool, Value) {
    let Value::Object(map) = args else { panic!("args must be an object") };
    let res = client
        .call_tool(CallToolRequestParams::new(tool.to_string()).with_arguments(map))
        .await
        .unwrap_or_else(|e| panic!("{tool}: {e}"));
    let text = res.content.iter().filter_map(|c| c.as_text().map(|t| t.text.clone())).collect::<String>();
    let ok = !res.is_error.unwrap_or(false);
    (ok, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_read_only_shared_mailboxes_with_a_valid_token() {
    let tmp = tempfile::tempdir().unwrap();
    let mail = tmp.path().join("mail");
    copy_dir(&testdata("library"), &mail);
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let eml = lib.add_path(&mail.join("eml")).unwrap()[0].id;
    let mbox = lib.add_path(&mail.join("archive.mbox")).unwrap()[0].id;
    // An unshared copy of the same emails in a recognisably named folder.
    copy_dir(&testdata("library/eml"), &mail.join("unshared").join("hidden-folder"));
    let hidden = lib.add_path(&mail.join("unshared")).unwrap()[0].id;
    wait_indexed(&lib);

    let shared: stv_mcp::Shared = Arc::new(RwLock::new(HashSet::from([eml])));
    let events: Arc<Mutex<Vec<AccessLogEntry>>> = Arc::default();
    let sink = events.clone();
    let log = AccessLog::new(tmp.path().join("access.log"), move |e| sink.lock().unwrap().push(e.clone()));
    let token = stv_mcp::new_token();
    let server = stv_mcp::start(lib.clone(), shared.clone(), log.clone(), 0, token.clone()).await.unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", server.addr.port());

    // Wrong token and browser-originated requests are refused before MCP sees them.
    let http = reqwest::Client::new();
    let init = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}});
    let post = |auth: &str, origin: Option<&str>| {
        let mut r = http
            .post(&url)
            .header("Authorization", auth)
            .header("Accept", "application/json, text/event-stream")
            .json(&init);
        if let Some(o) = origin {
            r = r.header("Origin", o);
        }
        r.send()
    };
    assert_eq!(post("Bearer wrong", None).await.unwrap().status(), 401);
    assert_eq!(post("", None).await.unwrap().status(), 401);
    assert_eq!(post(&format!("Bearer {token}"), Some("https://evil.example")).await.unwrap().status(), 403);
    // DNS rebinding: a foreign Host name resolving to 127.0.0.1 is refused.
    let rebound = http
        .post(&url)
        .header("Host", format!("evil.example:{}", server.addr.port()))
        .header("Authorization", format!("Bearer {token}"))
        .json(&init)
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403);
    // The auth scheme is case-insensitive (RFC 9110); oversized bodies are refused.
    assert_eq!(post(&format!("bearer {token}"), None).await.unwrap().status(), 200);
    let huge = http
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .body(vec![b' '; 1 << 20])
        .send()
        .await;
    // The server answers 413 without reading the rest of the body and closes the
    // connection; on Windows the client may see that close (WSAECONNABORTED) before the
    // response. Either way the request was refused.
    match huge {
        Ok(r) => assert_eq!(r.status(), 413),
        Err(e) => assert!(e.is_request() && !e.is_timeout(), "oversized body: {e}"),
    }

    // A real MCP client with the token.
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.as_str()).auth_header(token.clone()),
    );
    let client = ClientConfig::default().serve(transport).await.expect("connect");
    let tools: HashSet<String> =
        client.list_all_tools().await.unwrap().into_iter().map(|t| t.name.to_string()).collect();
    for t in [
        "list_mailboxes",
        "list_folders",
        "search",
        "list_messages",
        "read_message",
        "read_attachment",
        "get_thread",
        "list_attachments",
        "list_tagged",
    ] {
        assert!(tools.contains(t), "missing tool {t}: {tools:?}");
    }

    // Only the shared mailbox is listed and reachable.
    let (ok, v) = call(&client, "list_mailboxes", json!({})).await;
    assert!(ok);
    let ids: Vec<u64> = v["mailboxes"].as_array().unwrap().iter().map(|m| m["mailbox"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![u64::from(eml.0)]);
    let (ok, v) = call(&client, "list_messages", json!({ "mailbox": mbox.0 })).await;
    assert!(!ok, "unshared mailbox must be refused: {v}");

    // Search, then read the message and its embedded attachment.
    let (ok, v) = call(&client, "search", json!({ "query": "lighthouse" })).await;
    assert!(ok, "{v}");
    let hit = &v["results"][0];
    let id = hit["id"].as_str().expect("message id").to_string();
    let (ok, m) = call(&client, "read_message", json!({ "id": id })).await;
    assert!(ok, "{m}");
    let att = m["attachments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["embedded_message"] == true)
        .expect("embedded attachment");
    let (ok, a) = call(&client, "read_attachment", json!({ "id": id, "index": att["index"] })).await;
    assert!(ok, "{a}");
    assert!(a["text"].as_str().unwrap().contains("lighthouse"), "{a}");

    // Long bodies page by characters.
    let (_, short) = call(&client, "read_message", json!({ "id": id, "max_chars": 5 })).await;
    assert_eq!(short["body"].as_str().unwrap().chars().count(), 5);
    assert_eq!(short["body_continues"]["next_offset"], 5);

    // The thread holds only shared copies: the unshared duplicate contributes no entry,
    // no folder name and no preview.
    let (ok, t) = call(&client, "get_thread", json!({ "id": id })).await;
    assert!(ok, "{t}");
    let rows = t["conversation"].as_array().unwrap();
    assert!(!rows.is_empty() && rows.iter().all(|r| r["mailbox"] == eml.0), "{t}");
    assert!(rows.iter().any(|r| r["is_requested_message"] == true), "{t}");
    assert!(!t.to_string().contains("hidden-folder") && !t.to_string().contains(&format!("\"mailbox\":{}", hidden.0)));

    // Absurd offsets are refused instead of reaching the index; unknown sort values are
    // rejected by the argument schema before the tool runs.
    let (ok, e) = call(&client, "search", json!({ "query": "", "offset": u64::MAX })).await;
    assert!(!ok, "{e}");
    let bad_sort = CallToolRequestParams::new("search")
        .with_arguments(json!({ "query": "", "sort": "sideways" }).as_object().unwrap().clone());
    if let Ok(r) = client.call_tool(bad_sort).await {
        assert_eq!(r.is_error, Some(true), "{r:?}");
    }

    // Ids from an unshared mailbox are refused even if guessed; un-sharing applies live.
    let (ok, _) = call(&client, "list_folders", json!({ "mailbox": eml.0 })).await;
    assert!(ok);
    shared.write().unwrap().clear();
    let (ok, _) = call(&client, "read_message", json!({ "id": id })).await;
    assert!(!ok);

    // Every one of the 10 tool calls that ran was logged, with the agent's name.
    let logged = events.lock().unwrap().clone();
    assert_eq!(logged.len(), 10, "{logged:?}");
    assert!(logged.iter().any(|e| e.tool == "search" && e.ok && e.items >= 1));
    assert!(logged.iter().any(|e| e.tool == "list_messages" && !e.ok));
    assert!(logged.iter().all(|e| e.client.is_some()));
    assert_eq!(std::fs::read_to_string(tmp.path().join("access.log")).unwrap().lines().count(), 10);

    // Restart on the same port with a new token while the agent session is still open
    // (what "Regenerate token" does): the port is released and rebound, old token refused.
    let port = server.addr.port();
    server.shutdown().await;
    let token2 = stv_mcp::new_token();
    let server =
        stv_mcp::start(lib.clone(), shared.clone(), log.clone(), port, token2.clone()).await.expect("rebind same port");
    assert_eq!(post(&format!("Bearer {token}"), None).await.unwrap().status(), 401);
    assert_eq!(post(&format!("Bearer {token2}"), None).await.unwrap().status(), 200);
    drop(client);
    server.shutdown().await;
    assert!(http.post(&url).send().await.is_err(), "port closed after shutdown");

    // An empty or short token would disable or weaken auth: refused outright.
    assert!(stv_mcp::start(lib.clone(), shared.clone(), log.clone(), 0, String::new()).await.is_err());
}

/// A forwarded phishing mail: the inner message is a `message/rfc822` part with its own
/// attachment, a deceptive link and delivery headers.
const FORWARDED_PHISH: &str = "From: Forwarder <fwd@example.org>\r
To: Security <security@example.org>\r
Subject: FW: suspicious mail\r
Date: Mon, 2 Nov 2026 11:00:00 +0000\r
Message-ID: <outer@example.org>\r
MIME-Version: 1.0\r
Content-Type: multipart/mixed; boundary=\"outer\"\r
\r
--outer\r
Content-Type: text/plain\r
\r
See attached.\r
--outer\r
Content-Type: message/rfc822\r
Content-Disposition: attachment; filename=\"phish.eml\"\r
\r
Return-Path: <bounce@mailer.evil.example>\r
Received: from mx.example.org (mx.example.org [10.0.0.5]) by store.example.org with ESMTPS id Y;\r
 Mon, 2 Nov 2026 10:00:02 +0000\r
Received: from mailer.evil.example (unknown [203.0.113.7]) by mx.example.org with ESMTP id X;\r
 Mon, 2 Nov 2026 10:00:00 +0000\r
Authentication-Results: mx.example.org; spf=fail smtp.mailfrom=evil.example;\r
 dkim=none; dmarc=fail header.from=example.org\r
From: IT Support <it@example.org>\r
To: User <user@example.org>\r
Subject: Password expiry\r
Date: Mon, 2 Nov 2026 10:00:00 +0000\r
Message-ID: <inner@evil.example>\r
MIME-Version: 1.0\r
Content-Type: multipart/mixed; boundary=\"inner\"\r
\r
--inner\r
Content-Type: text/html; charset=utf-8\r
\r
<p>Refer to attached document for instructions.\r
<a href=\"http://login.evil.example/x\">https://portal.example.org</a></p>\r
--inner\r
Content-Type: text/plain; name=\"instructions.txt\"\r
Content-Disposition: attachment; filename=\"instructions.txt\"\r
\r
Step 1: enter your password at the portal.\r
--inner--\r
--outer--\r
";

#[tokio::test(flavor = "multi_thread")]
async fn attached_emails_open_with_links_transport_and_attachments() {
    let tmp = tempfile::tempdir().unwrap();
    let mail = tmp.path().join("mail");
    std::fs::create_dir_all(&mail).unwrap();
    std::fs::write(mail.join("forwarded.eml"), FORWARDED_PHISH).unwrap();
    let lib = Library::open(&tmp.path().join("lib")).unwrap();
    let src = lib.add_path(&mail).unwrap()[0].id;
    wait_indexed(&lib);

    let shared: stv_mcp::Shared = Arc::new(RwLock::new(HashSet::from([src])));
    let log = AccessLog::new(tmp.path().join("access.log"), |_| {});
    let token = stv_mcp::new_token();
    let server = stv_mcp::start(lib.clone(), shared.clone(), log, 0, token.clone()).await.unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", server.addr.port());
    let transport = StreamableHttpClientTransport::from_config(
        StreamableHttpClientTransportConfig::with_uri(url.as_str()).auth_header(token),
    );
    let client = ClientConfig::default().serve(transport).await.expect("connect");

    let (ok, list) = call(&client, "list_messages", json!({ "mailbox": src.0 })).await;
    assert!(ok, "{list}");
    let outer_id = list["messages"][0]["id"].as_str().unwrap().to_string();

    // The outer message names the attached email by its own id.
    let (ok, outer) = call(&client, "read_message", json!({ "id": outer_id })).await;
    assert!(ok, "{outer}");
    assert!(outer["embedded_in"].is_null());
    let att = outer["attachments"].as_array().unwrap().iter().find(|a| a["embedded_message"] == true).unwrap().clone();
    let inner_id = att["message_id"].as_str().expect("embedded message id").to_string();

    // read_attachment on it points at the same id, with a summary.
    let (ok, ra) = call(&client, "read_attachment", json!({ "id": outer_id, "index": att["index"] })).await;
    assert!(ok, "{ra}");
    assert_eq!(ra["embedded_message"]["message_id"], inner_id.as_str());
    assert_eq!(ra["embedded_message"]["subject"], "Password expiry");
    assert_eq!(ra["embedded_message"]["attachments"], 1);

    // The inner message: deceptive link, delivery facts, its own attachment, its parent.
    let (ok, inner) = call(&client, "read_message", json!({ "id": inner_id })).await;
    assert!(ok, "{inner}");
    assert_eq!(inner["subject"], "Password expiry");
    assert_eq!(inner["embedded_in"]["message_id"], outer_id.as_str());
    let link = &inner["links"][0];
    assert_eq!(link["url"], "http://login.evil.example/x", "{inner}");
    assert_eq!(link["text"], "https://portal.example.org");
    assert_eq!(link["mismatch"], true);
    let t = &inner["transport"];
    assert_eq!(t["return_path"], "<bounce@mailer.evil.example>", "{t}");
    assert_eq!(t["received_hops"], 2);
    assert_eq!(t["first_external_hop"]["ip"], "203.0.113.7");
    assert_eq!(t["authentication"]["spf"], "fail");
    assert_eq!(t["authentication"]["dmarc"], "fail");
    let nested = inner["attachments"].as_array().unwrap().iter().find(|a| a["name"] == "instructions.txt").unwrap();
    assert!(nested.get("message_id").is_none());

    // Nested attachment text, and the thread, both through the embedded id.
    let (ok, txt) = call(&client, "read_attachment", json!({ "id": inner_id, "index": nested["index"] })).await;
    assert!(ok, "{txt}");
    assert!(txt["text"].as_str().unwrap().contains("Step 1"), "{txt}");
    let (ok, thread) = call(&client, "get_thread", json!({ "id": inner_id })).await;
    assert!(ok, "{thread}");

    // Embedded ids obey sharing like any other id.
    shared.write().unwrap().clear();
    let (ok, _) = call(&client, "read_message", json!({ "id": inner_id })).await;
    assert!(!ok);
    let (ok, _) = call(&client, "read_attachment", json!({ "id": inner_id, "index": nested["index"] })).await;
    assert!(!ok);
    drop(client);
    server.shutdown().await;
}
