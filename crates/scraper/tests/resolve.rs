//! Profile resolution and optional WARC recording against local HTTP servers.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use archivindex_substack_scraper::resolve::{Config, Handle, Resolver};
use archivindex_warc::io::read::WarcReader;
use archivindex_warc::record::Record;
use archivindex_warc::record::extension::NoExtension;
use serde_json::json;

fn config() -> Config {
    let mut config = Config {
        delay: Duration::ZERO,
        timeout: Duration::from_secs(2),
        ..Config::default()
    };
    config.retry.attempts = 1;
    config.retry.initial_backoff = Duration::from_millis(1);
    config.retry.max_backoff = Duration::from_secs(2);
    config
}

fn response(status: u16, body: &str) -> (u16, String, String) {
    (status, String::new(), body.into())
}

fn redirect(location: &str) -> (u16, String, String) {
    (
        301,
        format!("Location: {location}\r\n"),
        "untransferred HEAD body".into(),
    )
}

fn records(path: &Path, gzip: bool) -> Vec<Record> {
    if gzip {
        WarcReader::from_path_gzip(path)
            .unwrap()
            .iter_records::<NoExtension>()
            .records()
            .collect::<Result<_, _>>()
            .unwrap()
    } else {
        WarcReader::from_path(path)
            .unwrap()
            .iter_records::<NoExtension>()
            .records()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

#[test]
fn handle_lookup_records_head_without_following_the_redirect() {
    for gzip in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(if gzip {
            "lookup.warc.gz"
        } else {
            "lookup.warc"
        });
        let (base, server) = common::server(vec![redirect("/@example/")]);
        let mut resolver = Resolver::new(config(), &base).unwrap();
        assert_eq!(
            resolver.resolve_handle(42, Some(&path)).unwrap(),
            Some(Handle::Set("example".into()))
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("HEAD /profile/42 HTTP/1.1\r\n"));
        assert!(!requests[0].to_ascii_lowercase().contains("cookie:"));
        let records = records(&path, gzip);
        let request = records
            .iter()
            .find_map(|record| match record {
                Record::Request { body, .. } => Some(body),
                _ => None,
            })
            .unwrap();
        assert!(request.starts_with(b"HEAD /profile/42 HTTP/1.1\r\n"));
        let (header, body) = records
            .iter()
            .find_map(|record| match record {
                Record::Response { header, body } => Some((header, body)),
                _ => None,
            })
            .unwrap();
        assert!(header.core.truncated.is_none());
        let metadata = archivindex_http::message::ResponseMetadata::parse(body).unwrap();
        assert_eq!(metadata.status, 301);
        assert_eq!(metadata.header("location"), Some(b"/@example/".as_slice()));
        assert!(body.ends_with(b"\r\n\r\n"));
        assert!(!String::from_utf8_lossy(body).contains("untransferred"));
    }
}

#[test]
fn missing_users_and_unset_handles_remain_distinct() {
    for (status, expected) in [(404, None), (200, Some(Handle::Unset))] {
        let (base, server) = common::server(vec![response(status, "")]);
        assert_eq!(
            Resolver::new(config(), &base)
                .unwrap()
                .resolve_handle(42, None)
                .unwrap(),
            expected
        );
        assert_eq!(server.join().unwrap().len(), 1);
        for slug in [false, true] {
            let (base, server) = common::server(vec![response(status, "")]);
            let mut resolver = Resolver::new(config(), &base).unwrap();
            if slug {
                assert!(resolver.resolve_slug(42, None).unwrap().is_none());
            } else {
                assert!(resolver.resolve_user(42, None).unwrap().is_none());
            }
            assert_eq!(server.join().unwrap().len(), 1);
        }
    }
}

#[test]
fn absolute_redirects_require_the_same_origin_and_no_credentials() {
    for status in [301, 302, 303, 307, 308] {
        let (base, server) = common::server_with(|base| {
            vec![(
                status,
                format!("Location: {base}@example\r\n"),
                String::new(),
            )]
        });
        assert_eq!(
            Resolver::new(config(), &base)
                .unwrap()
                .resolve_handle(42, None)
                .unwrap(),
            Some(Handle::Set("example".into()))
        );
        assert_eq!(server.join().unwrap().len(), 1);
    }
    for replacement in ["https://", "http://user:secret@"] {
        let (base, server) = common::server_with(|base| {
            vec![redirect(&format!(
                "{}@example",
                base.replace("http://", replacement)
            ))]
        });
        assert!(
            Resolver::new(config(), &base)
                .unwrap()
                .resolve_handle(42, None)
                .is_err()
        );
        assert_eq!(server.join().unwrap().len(), 1);
    }
}

#[test]
fn profile_and_slug_use_the_handle_and_preserve_the_complete_profile() {
    let profile = json!({"id": 42, "slug": "different-slug", "handle": "example",
        "name": null, "primaryPublication": {"future": [1, null]}, "subscriptions": []});
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.warc");
    let (base, server) = common::server(vec![
        redirect("/@example"),
        response(200, &profile.to_string()),
    ]);
    let mut resolver = Resolver::new(config(), &base).unwrap();
    let actual = resolver.resolve_user(42, Some(&path)).unwrap().unwrap();
    assert_eq!(serde_json::to_value(actual).unwrap(), profile);
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].starts_with("GET /api/v1/user/example/public_profile HTTP/1.1\r\n"));
    let records = records(&path, false);
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, Record::Request { .. }))
            .count(),
        2
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, Record::Response { .. }))
            .count(),
        2
    );
    for slug in ["different-slug", "", "non-ASCII-名前"] {
        let (base, server) = common::server(vec![
            redirect("/@example"),
            response(200, &json!({"id": 42, "slug": slug}).to_string()),
        ]);
        let result = Resolver::new(config(), &base)
            .unwrap()
            .resolve_slug(42, None)
            .unwrap();
        assert_eq!(result.as_deref(), (!slug.is_empty()).then_some(slug));
        server.join().unwrap();
    }
}

#[test]
fn malformed_redirects_are_errors_and_are_retained() {
    for location in [
        "/@",
        "/@.",
        "/@..",
        "/profile/42-name",
        "/@example/path",
        "/@example//",
        "/@example?query",
        "/@example#fragment",
        "/@a%2fb",
        "https://elsewhere.example/@example",
        "http://127.0.0.1:1/@example",
        "ftp://127.0.0.1/@example",
        "http://user:secret@127.0.0.1/@example",
        "http://[invalid",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.warc");
        let (base, server) = common::server(vec![redirect(location)]);
        assert!(
            Resolver::new(config(), &base)
                .unwrap()
                .resolve_user(42, Some(&path))
                .is_err(),
            "{location}"
        );
        assert_eq!(server.join().unwrap().len(), 1);
        assert!(
            records(&path, false)
                .iter()
                .any(|record| matches!(record, Record::Response { .. }))
        );
    }
    for status in [204, 301, 304, 401, 403, 429, 500, 503] {
        let (base, server) = common::server(vec![response(status, "")]);
        assert!(
            Resolver::new(config(), &base)
                .unwrap()
                .resolve_handle(42, None)
                .is_err(),
            "{status}"
        );
        server.join().unwrap();
    }
}

#[test]
fn profile_failures_are_not_reported_as_absence() {
    for (status, body) in [
        (200, "not JSON"),
        (200, r#"{"error":"unexpected"}"#),
        (200, r#"{"id":43,"slug":"someone-else"}"#),
        (204, ""),
        (302, ""),
        (403, ""),
        (500, ""),
    ] {
        let (base, server) = common::server(vec![redirect("/@example"), response(status, body)]);
        assert!(
            Resolver::new(config(), &base)
                .unwrap()
                .resolve_user(42, None)
                .is_err()
        );
        server.join().unwrap();
    }
    let (base, server) = common::server(vec![redirect("/@example"), response(404, "")]);
    assert!(
        Resolver::new(config(), &base)
            .unwrap()
            .resolve_user(42, None)
            .unwrap()
            .is_none()
    );
    server.join().unwrap();
}

#[test]
fn retries_record_every_attempt_and_honor_retry_after() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("retried.warc");
    let mut config = config();
    config.retry.attempts = 2;
    let (base, server) = common::server(vec![
        (429, "Retry-After: 1\r\n".into(), String::new()),
        redirect("/@example"),
        (503, "Retry-After: 0\r\n".into(), "busy".into()),
        response(200, r#"{"id":42,"slug":"example"}"#),
    ]);
    let start = Instant::now();
    assert!(
        Resolver::new(config, &base)
            .unwrap()
            .resolve_user(42, Some(&path))
            .unwrap()
            .is_some()
    );
    assert!(start.elapsed() >= Duration::from_secs(1));
    assert_eq!(server.join().unwrap().len(), 4);
    let statuses: Vec<_> = records(&path, false)
        .iter()
        .filter_map(|record| match record {
            Record::Response { body, .. } => Some(
                archivindex_http::message::ResponseMetadata::parse(body)
                    .unwrap()
                    .status,
            ),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, [429, 301, 503, 200]);
}

#[test]
fn request_spacing_covers_retries_profile_fetches_and_later_lookups() {
    let mut settings = config();
    settings.delay = Duration::from_millis(50);
    settings.retry.attempts = 2;
    let (base, server) = common::server(vec![
        (429, "Retry-After: 0\r\n".into(), String::new()),
        redirect("/@example"),
        response(200, r#"{"id":42,"slug":"example"}"#),
        response(200, ""),
    ]);
    let mut resolver = Resolver::new(settings, &base).unwrap();
    let start = Instant::now();
    assert!(resolver.resolve_user(42, None).unwrap().is_some());
    assert_eq!(
        resolver.resolve_handle(43, None).unwrap(),
        Some(Handle::Unset)
    );
    assert!(start.elapsed() >= Duration::from_millis(150));
    assert_eq!(server.join().unwrap().len(), 4);
}

#[test]
fn exhausted_retries_and_truncated_profiles_are_errors() {
    let mut settings = config();
    settings.retry.attempts = 2;
    let (base, server) = common::server(vec![response(429, ""), response(429, "")]);
    assert!(
        Resolver::new(settings, &base)
            .unwrap()
            .resolve_handle(42, None)
            .is_err()
    );
    assert_eq!(server.join().unwrap().len(), 2);

    // The captured prefix can be valid JSON even though the response was cut short in whitespace.
    let body = format!(
        "{}{}",
        json!({"id": 42, "slug": "example"}),
        " ".repeat(4096)
    );
    let mut settings = config();
    settings.max_response_bytes = 512;
    let (base, server) = common::server(vec![redirect("/@example"), response(200, &body)]);
    assert!(
        Resolver::new(settings, &base)
            .unwrap()
            .resolve_user(42, None)
            .is_err()
    );
    server.join().unwrap();
}

#[test]
fn configuration_and_output_are_validated_before_network_access() {
    for base in [
        "example",
        "https://x/path",
        "https://user:secret@x",
        "https://x/?query",
        "https://x/#fragment",
        "ftp://x",
    ] {
        assert!(Resolver::new(config(), base).is_err());
    }
    for settings in [
        Config {
            timeout: Duration::ZERO,
            ..config()
        },
        Config {
            max_response_bytes: 0,
            ..config()
        },
    ] {
        assert!(Resolver::new(settings, "http://127.0.0.1:1/").is_err());
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing.warc");
    std::fs::write(&path, "keep").unwrap();
    assert!(
        Resolver::new(config(), "http://127.0.0.1:1/")
            .unwrap()
            .resolve_handle(42, Some(&path))
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
}
