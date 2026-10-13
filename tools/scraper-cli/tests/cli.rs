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

#[test]
fn resolution_commands_validate_arguments_and_protect_existing_output() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing.warc");
    std::fs::write(&path, "keep").unwrap();
    for command in ["resolve-handle", "resolve-slug", "resolve-user"] {
        let help = cli().args([command, "--help"]).output().unwrap();
        assert!(help.status.success());
        let help = String::from_utf8(help.stdout).unwrap();
        assert!(help.contains("<USER_ID>"));
        assert!(help.contains("--output"));
        assert!(help.contains("--proxy"));
        assert!(help.contains("--show-ip"));
        for arguments in [
            vec![],
            vec!["not-an-id"],
            vec!["-1"],
            vec!["18446744073709551616"],
            vec!["42", "--delay=NaN"],
            vec!["42", "--delay=-1"],
            vec!["42", "--delay=inf"],
            vec!["42", "--gzip"],
        ] {
            let output = cli().arg(command).args(arguments).output().unwrap();
            assert!(!output.status.success());
            assert_eq!(output.stdout, b"");
        }
        let output = cli()
            .args([command, "42", "--output"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
    }
}

#[test]
fn invalid_proxies_stop_capture_without_recording_or_exposing_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("must-not-exist.warc");
    for command in [
        "resolve-handle",
        "resolve-slug",
        "resolve-user",
        "lookup-handle",
        "archive",
    ] {
        for proxy in [
            "https://username:secret@localhost",
            "socks5h://username:secret@localhost:0",
            "socks5h://username:secret@localhost/path",
            "socks5h://username:secret@localhost?query",
        ] {
            for show_ip in [false, true] {
                let mut cmd = cli();
                cmd.arg(command);
                if command == "archive" {
                    cmd.args(["--publication", "example"]);
                } else {
                    cmd.arg("42");
                }
                cmd.args(["--proxy", proxy, "--output"]).arg(&path);
                if show_ip {
                    cmd.arg("--show-ip");
                }
                let output = cmd.output().unwrap();
                assert!(!output.status.success());
                assert_eq!(output.stdout, b"");
                let error = String::from_utf8_lossy(&output.stderr);
                assert!(!error.contains("username"));
                assert!(!error.contains("secret"));
                assert!(!path.exists());
            }
        }
    }
}

#[test]
fn lookup_handle_accepts_shared_options_and_rejects_invalid_inputs() {
    let help = cli().args(["lookup-handle", "--help"]).output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    for argument in [
        "<HANDLE>",
        "--proxy",
        "--show-ip",
        "--output",
        "--delay",
        "--gzip",
    ] {
        assert!(help.contains(argument), "{help}");
    }
    for arguments in [
        vec![],
        vec![""],
        vec!["@"],
        vec!["@@example"],
        vec!["@../escape"],
        vec!["@example", "--delay=NaN"],
        vec!["@example", "--gzip"],
    ] {
        let output = cli().arg("lookup-handle").args(arguments).output().unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing.warc");
    std::fs::write(&path, "keep").unwrap();
    for handle in ["example", "@example"] {
        let output = cli()
            .args(["lookup-handle", handle, "--output"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
    }
}

#[test]
fn failed_ip_check_uses_the_proxy_and_stops_before_substack_requests() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    for command in [
        "resolve-handle",
        "resolve-slug",
        "resolve-user",
        "lookup-handle",
        "archive",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let proxy = format!("socks5h://{}", listener.local_addr().unwrap());
        let accepting = listener.try_clone().unwrap();
        let server = std::thread::spawn(move || {
            let start = Instant::now();
            let mut stream = loop {
                match accepting.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(start.elapsed() < Duration::from_secs(5));
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("proxy accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut greeting = [0; 3];
            stream.read_exact(&mut greeting).unwrap();
            assert_eq!(greeting, [5, 1, 0]);
            stream.write_all(&[5, 0]).unwrap();
            let mut header = [0; 5];
            stream.read_exact(&mut header).unwrap();
            assert_eq!(&header[..4], &[5, 1, 0, 3]);
            let mut host = vec![0; usize::from(header[4])];
            stream.read_exact(&mut host).unwrap();
            assert_eq!(host, b"api.ipify.org");
            let mut port = [0; 2];
            stream.read_exact(&mut port).unwrap();
            assert_eq!(u16::from_be_bytes(port), 443);
            stream.write_all(&[5, 2, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("must-not-exist.warc");
        let mut cmd = cli();
        cmd.arg(command);
        if command == "archive" {
            cmd.args(["--publication", "example"]);
        } else {
            cmd.arg("42");
        }
        let output = cmd
            .args(["--show-ip", "--proxy", &proxy, "--output"])
            .arg(&path)
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
        assert!(!path.exists());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Public IP address:"));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
