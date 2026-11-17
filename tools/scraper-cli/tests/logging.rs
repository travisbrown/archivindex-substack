//! CLI diagnostics stay on stderr and follow the shared verbosity flags.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_archivindex-substack-scraper"))
}

#[test]
fn errors_remain_visible_at_every_verbosity_without_changing_exit_status() {
    for flags in [vec![], vec!["-q"], vec!["-v"], vec!["-vv"], vec!["-vvv"]] {
        let output = cli()
            .args(flags)
            .args(["lookup-handle", "@", "--delay=NaN"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(output.stdout, b"");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("ERROR"), "{stderr}");
        assert!(
            stderr.contains("delay must be finite and nonnegative"),
            "{stderr}"
        );
        assert!(!stderr.contains('\u{1b}'), "{stderr}");
    }
}

#[test]
fn quiet_and_verbose_conflict_across_subcommand_boundaries() {
    let output = cli()
        .args(["-q", "inspect", "unused.warc", "-v"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stdout, b"");
    assert!(String::from_utf8_lossy(&output.stderr).contains("cannot be used with"));
}

#[test]
fn verbosity_filters_archive_summaries_and_preserves_json_output() {
    for flags in [vec![], vec!["--quiet"], vec!["-v"]] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.warc");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let child = cli()
            .args(["archive", "--publication", &origin, "--delay=0", "--output"])
            .arg(&path)
            .args(&flags)
            .env_remove("SUBSTACK_COOKIE")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no archive request received");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = BufReader::new(&stream);
        let mut line = String::new();
        loop {
            line.clear();
            assert!(request.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
        }
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n[]")
            .unwrap();
        drop(stream);
        let output = child.wait_with_output().unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        assert_eq!(output.stdout, b"");
        if flags == ["-v"] {
            assert!(stderr.contains("INFO"), "{stderr}");
            assert!(stderr.contains("Archived 0 post lookups to"), "{stderr}");
        } else {
            assert!(stderr.is_empty(), "{stderr}");
        }

        let output = cli()
            .args(&flags)
            .args(["export", "--include-pages"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stderr, b"");
        let row: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(row["kind"], "archive");
        assert_eq!(row["data"], serde_json::json!([]));
    }
}
