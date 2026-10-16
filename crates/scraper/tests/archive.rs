//! Capture and resume against deterministic local HTTP servers, without live Substack access.

mod common;

use std::collections::BTreeSet;
use std::net::TcpListener;
use std::path::Path;
use std::time::Duration;

use archivindex_substack_scraper::{
    CaptureKind, Config, Downloader, Error, ResumeState, read_archive,
};
use archivindex_warc::io::read::WarcReader;
use archivindex_warc::io::write::WarcWriter;
use archivindex_warc::record::Record;
use archivindex_warc::record::extension::NoExtension;
use chrono::Utc;
use serde_json::{Value, json};

fn post(id: u64, slug: &str) -> Value {
    json!({"id": id, "publication_id": 1, "slug": slug, "title": slug,
        "body_html": "<p>Example content</p>", "future": null})
}

fn page(posts: &[Value]) -> String {
    serde_json::to_string(posts).unwrap()
}

fn server(responses: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
    common::server(
        responses
            .into_iter()
            .map(|(status, body)| {
                (
                    status,
                    "Retry-After: 3600\r\nLocation: https://other.example/\r\n".into(),
                    body,
                )
            })
            .collect(),
    )
}

fn config(gzip: bool) -> Config {
    Config {
        delay: Duration::ZERO,
        gzip,
        cookie: Some("substack.sid=test-secret".into()),
        ..Config::default()
    }
}

fn records(path: &Path, gzip: bool) -> Result<Vec<Record>, Error> {
    if gzip {
        Ok(WarcReader::from_path_gzip(path)?
            .iter_records::<NoExtension>()
            .records()
            .collect::<Result<_, _>>()?)
    } else {
        Ok(WarcReader::from_path(path)?
            .iter_records::<NoExtension>()
            .records()
            .collect::<Result<_, _>>()?)
    }
}

#[test]
fn paginates_short_pages_archives_missing_posts_and_links_records() -> Result<(), Error> {
    for gzip in [false, true] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("segment.data");
        let one = post(1, "one");
        let two = post(2, "two");
        let (base, handle) = server(vec![
            (200, page(std::slice::from_ref(&one))),
            (200, one.to_string()),
            (200, page(std::slice::from_ref(&two))),
            (404, "{}".into()),
            (200, "[]".into()),
        ]);
        let downloader = Downloader::new(config(gzip), &base)?;
        let mut progress = Vec::new();
        assert_eq!(
            downloader
                .archive_with_progress(&ResumeState::default(), &path, |n| progress.push(n))?,
            2
        );
        assert_eq!(progress, [0, 1, 2]);
        let requests = handle.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests[0].starts_with("GET /api/v1/archive?sort=new&offset=0&limit=20 HTTP/1.1"));
        assert!(requests[2].contains("offset=1&limit=20"));
        assert!(requests[4].contains("offset=2&limit=20"));
        let contents = read_archive(&path)?;
        assert_eq!(contents.resume.completed_posts(), 2);
        assert_eq!(contents.captures.len(), 4);
        assert_eq!(contents.captures[1].data, one);
        assert_eq!(contents.captures[1].kind, CaptureKind::Post);
        assert_eq!(contents.status_counts[&404], 1);
        let records = records(&path, gzip)?;
        assert_eq!(records.len(), 16);
        assert!(matches!(records[0], Record::Warcinfo { .. }));
        let warcinfo_id = &records[0].core().record_id;
        let mut unique = BTreeSet::new();
        for record in &records {
            assert!(unique.insert(record.core().record_id.to_string()));
        }
        for (index, group) in records[1..].as_chunks::<3>().0.iter().enumerate() {
            let [request, response, metadata] = group;
            for record in group {
                assert_eq!(record.warcinfo_id(), Some(warcinfo_id));
            }
            let Record::Request { header, body } = request else {
                panic!("expected request")
            };
            let archived = archivindex_http::message::RequestMetadata::parse(body).unwrap();
            let sent =
                archivindex_http::message::RequestMetadata::parse(requests[index].as_bytes())
                    .unwrap();
            assert_eq!(archived.method(), sent.method());
            assert_eq!(archived.target(), sent.target());
            for name in ["host", "user-agent", "accept", "accept-encoding", "cookie"] {
                assert_eq!(archived.header(name), sent.header(name));
            }
            assert_eq!(
                archived.header("cookie"),
                Some(b"substack.sid=test-secret".as_slice())
            );
            assert!(matches!(response, Record::Response { .. }));
            assert_eq!(response.concurrent_to(), [request.core().record_id.clone()]);
            let Record::Metadata {
                header: meta_header,
                body: meta_body,
            } = metadata
            else {
                panic!("expected metadata")
            };
            assert_eq!(meta_header.target_uri.as_ref(), Some(&header.target_uri));
            assert_eq!(
                metadata.concurrent_to(),
                [response.core().record_id.clone()]
            );
            let archivindex_warc::record::FieldsBlock::Fields(fields) = meta_body else {
                panic!("expected fields")
            };
            assert!(fields.fetch_time_ms().is_some());
        }
        let before = std::fs::read(&path)?;
        assert!(downloader.archive(&ResumeState::default(), &path).is_err());
        assert_eq!(std::fs::read(&path)?, before);
    }
    Ok(())
}

#[test]
fn resume_recovers_discovered_posts_and_restarts_listings() -> Result<(), Error> {
    let dir = tempfile::tempdir()?;
    let one = post(1, "one");
    let two = post(2, "two");
    let new = post(3, "new");
    let (base, handle) = server(vec![
        (200, page(&[one.clone(), two.clone()])),
        (200, one.to_string()),
        // Resume must fetch two even though it no longer appears in the live listing.
        (200, two.to_string()),
        (200, page(&[new.clone(), one.clone()])),
        (200, new.to_string()),
        (200, "[]".into()),
        // Fully resumed runs still scan current listing pages, but never repeat post lookups.
        (200, page(&[new, one])),
        (200, "[]".into()),
    ]);
    let first = dir.path().join("first.warc");
    let mut limited = config(false);
    limited.max_posts = Some(1);
    assert_eq!(
        Downloader::new(limited, &base)?.archive(&ResumeState::default(), &first)?,
        1
    );
    let mut resume = read_archive(first)?.resume;
    let second = dir.path().join("second.warc.gz");
    assert_eq!(
        Downloader::new(config(true), &base)?.archive(&resume, &second)?,
        2
    );
    resume.extend(read_archive(second)?.resume);
    assert_eq!(resume.completed_posts(), 3);
    assert_eq!(
        Downloader::new(config(false), &base)?.archive(&resume, dir.path().join("third.warc"))?,
        0
    );
    let requests = handle.join().unwrap();
    assert!(requests[2].starts_with("GET /api/v1/posts/two "));
    assert!(requests[3].contains("offset=0"));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.starts_with("GET /api/v1/posts/"))
            .count(),
        3
    );
    Ok(())
}

#[test]
fn http_errors_are_retained_and_retryable_including_listing_404() -> Result<(), Error> {
    for status in [401, 403, 429, 500, 302] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("failed.warc");
        let one = post(1, "one");
        let two = post(2, "two");
        let (base, handle) = server(vec![
            (200, page(&[one.clone(), two])),
            (200, one.to_string()),
            (status, "{}".into()),
        ]);
        let mut progress = Vec::new();
        let error = Downloader::new(config(false), &base)?
            .archive_with_progress(&ResumeState::default(), &path, |n| progress.push(n))
            .unwrap_err();
        assert!(error.to_string().contains(&status.to_string()), "{error}");
        assert_eq!(progress, [0, 1]);
        assert_eq!(handle.join().unwrap().len(), 3);
        let contents = read_archive(&path)?;
        assert_eq!(contents.resume.completed_posts(), 1);
        assert_eq!(contents.status_counts[&status], 1);
        let raw = std::fs::read_to_string(path)?;
        assert!(raw.contains(&format!("HTTP/1.1 {status}")));
        assert!(raw.contains("Retry-After: 3600"));
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("missing-listing.warc");
    let (base, handle) = server(vec![(404, "{}".into())]);
    assert!(
        Downloader::new(config(false), &base)?
            .archive(&ResumeState::default(), &path)
            .is_err()
    );
    handle.join().unwrap();
    assert_eq!(read_archive(path)?.resume.completed_posts(), 0);
    Ok(())
}

#[test]
fn failed_post_is_retried_from_an_earlier_listing() -> Result<(), Error> {
    let dir = tempfile::tempdir()?;
    let one = post(1, "one");
    let (base, handle) = server(vec![
        (200, page(std::slice::from_ref(&one))),
        (429, "{}".into()),
        (200, one.to_string()),
        (200, "[]".into()),
    ]);
    let downloader = Downloader::new(config(false), &base)?;
    let path = dir.path().join("first.warc");
    assert!(downloader.archive(&ResumeState::default(), &path).is_err());
    let resume = read_archive(path)?.resume;
    assert_eq!(resume.completed_posts(), 0);
    assert_eq!(
        downloader.archive(&resume, dir.path().join("next.warc"))?,
        1
    );
    let requests = handle.join().unwrap();
    assert_eq!(requests[1], requests[2]);
    Ok(())
}

#[test]
fn malformed_json_and_mismatched_slugs_do_not_complete_posts() -> Result<(), Error> {
    for body in [
        "<html>challenge</html>".into(),
        "{\"error\":\"unexpected\"}".into(),
        post(1, "other").to_string(),
    ] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("invalid.warc");
        let (base, handle) = server(vec![(200, page(&[post(1, "one")])), (200, body.clone())]);
        assert!(
            Downloader::new(config(false), &base)?
                .archive(&ResumeState::default(), &path)
                .is_err()
        );
        handle.join().unwrap();
        assert_eq!(read_archive(&path)?.resume.completed_posts(), 0);
        assert!(std::fs::read_to_string(path)?.contains(&body));
    }
    Ok(())
}

#[test]
fn duplicate_posts_are_skipped_and_repeated_pages_stop_the_run() -> Result<(), Error> {
    let dir = tempfile::tempdir()?;
    let one = post(1, "one");
    let listing = page(&[one.clone(), one.clone()]);
    let (base, handle) = server(vec![
        (200, listing.clone()),
        (200, one.to_string()),
        (200, listing),
    ]);
    let path = dir.path().join("repeated.warc");
    let error = Downloader::new(config(false), &base)?
        .archive(&ResumeState::default(), &path)
        .unwrap_err();
    assert!(error.to_string().contains("no progress"));
    assert_eq!(handle.join().unwrap().len(), 3);
    assert_eq!(read_archive(path)?.resume.completed_posts(), 1);
    Ok(())
}

#[test]
fn failures_before_response_headers_leave_a_readable_archive() -> Result<(), Error> {
    let dir = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let base = format!("http://{}/", listener.local_addr()?);
    drop(listener);
    let path = dir.path().join("transport.warc");
    assert!(
        Downloader::new(config(false), &base)?
            .archive(&ResumeState::default(), &path)
            .is_err()
    );
    let recorded = records(&path, false)?;
    assert_eq!(recorded.len(), 1);
    assert!(matches!(recorded[0], Record::Warcinfo { .. }));
    assert!(read_archive(path)?.captures.is_empty());
    Ok(())
}

fn write_response(
    writer: &mut WarcWriter<std::fs::File>,
    url: &str,
    status: u16,
    body: &str,
    length: Option<usize>,
) -> Result<(), Error> {
    let message = format!(
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\n\r\n{body}",
        length.unwrap_or(body.len())
    );
    let record: Record = Record::response(url, Utc::now())?.body(message.into_bytes())?;
    writer.write(&record.into_raw()?)?;
    Ok(())
}

#[test]
fn reader_keeps_versions_and_rejects_unrelated_incomplete_and_broken_records() -> Result<(), Error>
{
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("records.warc");
    let mut writer = WarcWriter::new(std::fs::File::create(&path)?);
    let one = post(1, "one");
    let mut changed = one.clone();
    changed["title"] = "Changed".into();
    for (url, status, body, length) in [
        (
            "https://example.substack.com/api/v1/posts/one",
            200,
            one.to_string(),
            None,
        ),
        (
            "https://example.substack.com/api/v1/posts/one",
            200,
            changed.to_string(),
            None,
        ),
        (
            "https://example.substack.com/api/v1/posts/missing",
            404,
            "{}".into(),
            None,
        ),
        (
            "https://example.substack.com/api/v1/posts/incomplete",
            404,
            "{}".into(),
            Some(50),
        ),
        (
            "https://example.substack.com/api/v1/posts/one?preview=true",
            200,
            one.to_string(),
            None,
        ),
        (
            "https://example.substack.com/api/v1/other",
            200,
            one.to_string(),
            None,
        ),
        (
            "https://example.substack.com/api/v1/archive?sort=new&offset=0&limit=20&limit=10",
            200,
            "[]".into(),
            None,
        ),
    ] {
        write_response(&mut writer, url, status, &body, length)?;
    }
    writer.flush()?;
    let contents = read_archive(&path)?;
    assert_eq!(contents.resume.completed_posts(), 2);
    assert_eq!(contents.captures.len(), 2);
    assert_eq!(contents.captures[0].data, one);
    assert_eq!(contents.captures[1].data, changed);
    let mut bytes = std::fs::read(&path)?;
    bytes.truncate(bytes.len() - 8);
    std::fs::write(&path, bytes)?;
    assert!(read_archive(path).is_err());
    Ok(())
}

#[test]
fn resume_is_scoped_to_the_publication_origin() -> Result<(), Error> {
    let dir = tempfile::tempdir()?;
    let prior = dir.path().join("prior.warc");
    let mut writer = WarcWriter::new(std::fs::File::create(&prior)?);
    write_response(
        &mut writer,
        "https://other.example/api/v1/archive?sort=new&offset=0&limit=20",
        200,
        &page(&[post(1, "one")]),
        None,
    )?;
    write_response(
        &mut writer,
        "https://other.example/api/v1/posts/one",
        200,
        &post(1, "one").to_string(),
        None,
    )?;
    writer.flush()?;
    let one = post(1, "one");
    let (base, handle) = server(vec![
        (200, page(std::slice::from_ref(&one))),
        (200, one.to_string()),
        (200, "[]".into()),
    ]);
    assert_eq!(
        Downloader::new(config(false), &base)?
            .archive(&read_archive(prior)?.resume, dir.path().join("new.warc"))?,
        1
    );
    assert_eq!(handle.join().unwrap().len(), 3);
    Ok(())
}

#[test]
fn empty_publications_produce_a_readable_archive() -> Result<(), Error> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("empty.warc");
    let (base, handle) = server(vec![(200, "[]".into())]);
    assert_eq!(
        Downloader::new(config(false), &base)?.archive(&ResumeState::default(), &path)?,
        0
    );
    handle.join().unwrap();
    assert_eq!(records(&path, false)?.len(), 4);
    assert_eq!(read_archive(path)?.captures.len(), 1);
    Ok(())
}
