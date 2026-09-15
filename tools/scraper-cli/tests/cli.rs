//! Offline CLI export, inspection, and argument validation.

use std::process::Command;

use archivindex_warc::io::write::{Compression, WarcWriter};
use archivindex_warc::record::Record;
use chrono::Utc;
use serde_json::{Value, json};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_archivindex-substack-scraper"))
}

#[test]
fn export_preserves_nulls_and_versions_and_inspect_reports_failures() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fixture.data");
    let mut writer = WarcWriter::new(std::fs::File::create(&path).unwrap())
        .with_compression(Compression::gzip());
    let first = json!({"id": 1, "publication_id": 3, "slug": "one", "title": "First", "body_html": null, "unknown": {"a": 1}});
    let mut second = first.clone();
    second["title"] = "Second".into();
    for (endpoint, status, data) in [
        (
            "archive?sort=new&offset=0&limit=20",
            200,
            json!([first.clone()]),
        ),
        ("posts/one", 200, first.clone()),
        ("posts/one", 200, second.clone()),
        ("posts/missing", 404, json!({})),
        ("posts/failed", 429, json!({"error": "rate limited"})),
    ] {
        let body = data.to_string();
        let message = format!(
            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let record: Record = Record::response(
            &format!("https://example.substack.com/api/v1/{endpoint}"),
            Utc::now(),
        )
        .unwrap()
        .body(message.into_bytes())
        .unwrap();
        writer.write(&record.into_raw().unwrap()).unwrap();
    }
    writer.flush().unwrap();
    let output = cli().arg("export").arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let rows: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["data"], first);
    assert_eq!(rows[1]["data"], second);
    assert_eq!(rows[0]["kind"], "post");
    assert!(rows[0]["captured_at"].is_string());
    let output = cli()
        .arg("export")
        .arg("--include-pages")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 3);
    let output = cli().arg("inspect").arg(&path).output().unwrap();
    assert!(output.status.success());
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["posts"], 2);
    assert_eq!(summary["pages"], 1);
    assert_eq!(summary["completed_posts"], 2);
    assert_eq!(summary["http_statuses"]["429"], 1);
    let output = cli().arg("export").arg(&path).arg(&path).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 4);
}

#[test]
fn invalid_settings_fail_before_creating_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("must-not-exist.warc");
    for arguments in [
        vec!["--delay=NaN"],
        vec!["--delay=-1"],
        vec!["--delay=inf"],
        vec!["--page-size=0"],
        vec!["--page-size=101"],
        vec!["--max-posts=0"],
    ] {
        let output = cli()
            .args(["archive", "--publication", "example"])
            .arg("--output")
            .arg(&path)
            .args(arguments)
            .env_remove("SUBSTACK_COOKIE")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!path.exists());
    }
    let output = cli()
        .args(["archive", "--publication", "example"])
        .arg("--output")
        .arg(&path)
        .env("SUBSTACK_COOKIE", "secret\r\nInjected: bad")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!path.exists());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("secret"));
}
