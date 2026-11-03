//! Drives the `stv` binary end to end against the committed `testdata/library` fixtures.

use std::path::{Path, PathBuf};
use std::process::Command;

fn testdata(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata").join(rel)
}

fn stv(data: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_stv")).arg("--data-dir").arg(data).args(args).output().expect("run stv");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn ok(data: &Path, args: &[&str]) -> String {
    let (success, stdout, stderr) = stv(data, args);
    assert!(success, "stv {args:?} failed:\n{stdout}\n{stderr}");
    stdout
}

fn json(data: &Path, args: &[&str]) -> serde_json::Value {
    let mut a = vec!["--json"];
    a.extend_from_slice(args);
    serde_json::from_str(&ok(data, &a)).expect("valid JSON")
}

#[test]
fn add_index_search_show_thread_export() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("lib");
    let mail = tmp.path().join("mail");
    std::fs::create_dir_all(mail.join("eml")).unwrap();
    std::fs::copy(testdata("library/archive.mbox"), mail.join("archive.mbox")).unwrap();
    for f in ["b1.eml", "e1.eml"] {
        std::fs::copy(testdata("library/eml").join(f), mail.join("eml").join(f)).unwrap();
    }
    let mbox = mail.join("archive.mbox");
    let eml = mail.join("eml");

    let added = json(&data, &["add", mbox.to_str().unwrap(), eml.to_str().unwrap(), "--wait"]);
    let added = added.as_array().unwrap();
    assert_eq!(added.len(), 2);
    assert_eq!(added[0]["kind"], "mbox");
    assert_eq!(added[1]["kind"], "eml");
    assert_eq!(added[0]["index"]["done"], true);

    let human = ok(&data, &["sources"]);
    assert!(human.lines().next().unwrap().starts_with("ID"), "{human}");
    assert!(human.contains("archive.mbox"));
    let human = ok(&data, &["index", "--wait"]);
    assert_eq!(human.matches("indexed").count(), 2, "{human}");

    let res = json(&data, &["search", "zeppelin", "--limit", "10"]);
    assert_eq!(res["total"], 3, "{res}");
    let subjects: Vec<&str> =
        res["hits"].as_array().unwrap().iter().map(|h| h["header"]["subject"].as_str().unwrap()).collect();
    assert!(subjects.contains(&"Project kickoff"));

    let ls = json(&data, &["ls", "1", "--sort", "date"]);
    assert_eq!(ls["total"], 3);
    assert_eq!(ls["rows"][0]["subject"], "Project kickoff");
    let past_end = json(&data, &["ls", "1", "--offset", "10"]);
    assert_eq!((past_end["total"].as_u64(), past_end["rows"].as_array().map(Vec::len)), (Some(3), Some(0)));

    let show = ok(&data, &["show", "1", "#2", "--sort", "date"]);
    assert!(show.contains("Subject: Re: Project kickoff"), "{show}");
    assert!(show.contains("notes.txt"));
    let props = ok(&data, &["show", "1", "#2", "--sort", "date", "--props"]);
    assert!(props.contains("Subject") && props.contains("MIME header"), "{props}");
    let shown = json(&data, &["show", "1", "#2", "--sort", "date", "--props"]);
    let rows = shown["properties"].as_array().expect("properties array");
    assert!(rows.iter().any(|p| p["type_name"] == "MIME header" && p["tag"] == "Subject"), "{shown}");
    assert!(json(&data, &["show", "1", "#2", "--sort", "date"]).get("properties").is_none());

    // Flags after the words are still flags; `-word` excludes when quoted or after `--`.
    let names = |v: &serde_json::Value| -> Vec<String> {
        v["rows"].as_array().unwrap().iter().map(|r| r["filename"].as_str().unwrap().to_owned()).collect()
    };
    assert_eq!(names(&json(&data, &["attachments", "notes", "--limit", "5", "--sort", "size"])), ["notes.txt"]);
    for args in [&["attachments", "--limit", "50", "--", "-notes"][..], &["attachments", "csv -notes"]] {
        let found = names(&json(&data, args));
        assert!(found.iter().any(|n| n == "budget.csv") && !found.iter().any(|n| n == "notes.txt"), "{found:?}");
    }

    let thread = json(&data, &["thread", "1", "#1", "--sort", "date"]);
    let ids: Vec<&str> =
        thread.as_array().unwrap().iter().map(|t| t["header"]["message_id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["a1@stv.test", "b1@stv.test", "c1@stv.test"]);

    let out = tmp.path().join("export");
    let rep = json(&data, &["export", "zeppelin", "--format", "eml", "--out", out.to_str().unwrap()]);
    assert_eq!(rep["written"], 3);
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 3);

    let rep = json(&data, &["export", "--source", "1", "--format", "csv", "--out", out.to_str().unwrap()]);
    assert_eq!(rep["written"], 3);
    assert!(out.join("export.csv").exists());

    // Errors are reported with a non-zero exit code.
    let (success, _, stderr) = stv(&data, &["ls", "nope"]);
    assert!(!success && stderr.contains("no source matches"));
    // --folder only makes sense with --source (usage error).
    let (success, _, stderr) = stv(&data, &["export", "x", "--folder", "Inbox", "--format", "eml", "--out", "unused"]);
    assert!(!success && stderr.contains("--source"), "{stderr}");
    // A partially failed add still registers the rest, but exits non-zero.
    let missing = mail.join("missing.mbox");
    let (success, _, stderr) = stv(&data, &["add", mbox.to_str().unwrap(), missing.to_str().unwrap()]);
    assert!(!success && stderr.contains("1 of 2 path(s) could not be added"), "{stderr}");

    // A library is used by one process at a time.
    let held = stv_library::Library::open(&data).unwrap();
    let (success, _, stderr) = stv(&data, &["sources"]);
    assert!(!success && stderr.contains("in use by another STViewer process"), "{stderr}");
    drop(held);

    let bench = json(&data, &["bench", mbox.to_str().unwrap()]);
    assert_eq!(bench["headers"], 3);
    assert_eq!(bench["kind"], "mbox");
}
