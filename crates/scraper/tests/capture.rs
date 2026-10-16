//! Publication recording, truncation, and progress through archiver sessions.

mod common;

use std::time::{Duration, Instant};

use archivindex_substack_scraper::{Config, Downloader, ResumeState, read_archive};
use archivindex_warc::io::read::WarcReader;
use archivindex_warc::record::Record;
use archivindex_warc::record::extension::NoExtension;

const POST: &str = r#"{"id":1,"publication_id":1,"slug":"one","title":"One"}"#;

fn config() -> Config {
    Config {
        delay: Duration::ZERO,
        timeout: Duration::from_secs(2),
        ..Config::default()
    }
}

#[test]
fn truncated_json_and_404s_stop_without_completing_posts_or_following_listings() {
    for (listing, status) in [(true, 200), (false, 200), (false, 404)] {
        let mut responses = Vec::new();
        if !listing {
            responses.push((200, String::new(), format!("[{POST}]")));
        }
        let valid_prefix = if listing {
            format!("[{POST}]")
        } else {
            POST.into()
        };
        responses.push((
            status,
            String::new(),
            format!("{valid_prefix}{}", " ".repeat(4096)),
        ));
        let (base, server) = common::server(responses);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("truncated.warc");
        let mut settings = config();
        settings.max_response_bytes = 1024;
        let mut progress = Vec::new();
        let error = Downloader::new(settings, &base)
            .unwrap()
            .archive_with_progress(&ResumeState::default(), &path, |n| progress.push(n))
            .unwrap_err();
        assert!(error.to_string().contains("incomplete"), "{error}");
        assert_eq!(progress, [0]);
        assert_eq!(server.join().unwrap().len(), if listing { 1 } else { 2 });
        let contents = read_archive(&path).unwrap();
        assert_eq!(contents.resume.completed_posts(), 0);
        assert_eq!(contents.captures.len(), usize::from(!listing));
        let records = WarcReader::from_path(&path)
            .unwrap()
            .iter_records::<NoExtension>()
            .records()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(records.iter().any(|record| matches!(record, Record::Response { header, .. } if header.core.truncated.is_some())));
    }
}

#[test]
fn progress_sees_flushed_records_and_failed_publication_preserves_the_partial_file() {
    for fail_publication in [false, true] {
        let (base, server) = common::server(vec![
            (200, String::new(), format!("[{POST}]")),
            (200, String::new(), POST.into()),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("segment.warc.gz");
        let partial = dir.path().join("segment.warc.gz.partial");
        let mut settings = config();
        settings.max_posts = Some(1);
        let mut progress = Vec::new();
        let result = Downloader::new(settings, &base)
            .unwrap()
            .archive_with_progress(&ResumeState::default(), &path, |n| {
                progress.push(n);
                if n > 0 {
                    assert!(!path.exists());
                    assert_eq!(read_archive(&partial).unwrap().resume.completed_posts(), n);
                    if fail_publication {
                        std::fs::write(&path, "keep").unwrap();
                    }
                }
            });
        assert_eq!(progress, [0, 1]);
        assert_eq!(server.join().unwrap().len(), 2);
        if fail_publication {
            assert!(result.is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
            assert_eq!(read_archive(partial).unwrap().resume.completed_posts(), 1);
        } else {
            assert_eq!(result.unwrap(), 1);
            assert!(!partial.exists());
            assert_eq!(read_archive(path).unwrap().resume.completed_posts(), 1);
        }
    }
}

#[test]
fn existing_partial_output_is_preserved_before_network_access() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("segment.warc");
    let partial = dir.path().join("segment.warc.partial");
    std::fs::write(&partial, "keep").unwrap();
    assert!(
        Downloader::new(config(), "http://127.0.0.1:1/")
            .unwrap()
            .archive(&ResumeState::default(), &path)
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(partial).unwrap(), "keep");
    assert!(!path.exists());
}

#[test]
fn request_spacing_covers_listing_and_post_requests() {
    let (tx, rx) = std::sync::mpsc::channel();
    let (base, server) = common::server_with_connection(
        |_| {
            vec![
                (200, String::new(), format!("[{POST}]")),
                (200, String::new(), POST.into()),
                (200, String::new(), "[]".into()),
            ]
        },
        move |_| tx.send(Instant::now()).unwrap(),
    );
    let dir = tempfile::tempdir().unwrap();
    let mut settings = config();
    settings.delay = Duration::from_millis(100);
    Downloader::new(settings, &base)
        .unwrap()
        .archive(&ResumeState::default(), dir.path().join("segment.warc"))
        .unwrap();
    assert_eq!(server.join().unwrap().len(), 3);
    let times: Vec<_> = rx.into_iter().collect();
    for times in times.windows(2) {
        assert!(times[1].duration_since(times[0]) >= Duration::from_millis(90));
    }
}

#[test]
fn wire_framing_is_preserved_and_disconnected_bodies_remain_unfinished() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    for disconnected in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let listing = format!("[{POST}]");
        let listing_response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{listing}",
            listing.len()
        );
        let post_response = if disconnected {
            // A valid JSON prefix must still be rejected when Content-Length promises more bytes.
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{POST}",
                POST.len() + 1
            )
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{POST}\r\n0\r\n\r\n",
                POST.len()
            )
        };
        let responses = [listing_response, post_response.clone()];
        let server = std::thread::spawn(move || {
            for response in responses {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline);
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                for line in BufReader::new(&stream).lines() {
                    if line.unwrap().is_empty() {
                        break;
                    }
                }
                stream.write_all(response.as_bytes()).unwrap();
            }
            listener
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wire.warc");
        let mut settings = config();
        settings.max_posts = Some(1);
        let mut progress = Vec::new();
        let result = Downloader::new(settings, &base)
            .unwrap()
            .archive_with_progress(&ResumeState::default(), &path, |n| progress.push(n));
        let listener = server.join().unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let contents = read_archive(&path).unwrap();
        if disconnected {
            assert!(result.is_err());
            assert_eq!(progress, [0]);
            assert_eq!(contents.resume.completed_posts(), 0);
        } else {
            assert_eq!(result.unwrap(), 1);
            assert_eq!(progress, [0, 1]);
            assert_eq!(
                contents.captures[1].data,
                serde_json::from_str::<serde_json::Value>(POST).unwrap()
            );
            assert!(
                std::fs::read_to_string(path)
                    .unwrap()
                    .contains(&post_response)
            );
        }
    }
}
