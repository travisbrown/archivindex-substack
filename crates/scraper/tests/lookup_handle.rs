//! Direct handle lookups share profile validation and recording with numeric-ID resolution.

mod common;

use std::time::Duration;

use archivindex_substack_scraper::resolve::{Config, Resolver};
use archivindex_warc::io::read::WarcReader;
use archivindex_warc::record::Record;
use archivindex_warc::record::extension::NoExtension;
use serde_json::json;

fn resolver(base: &str) -> Resolver {
    let mut config = Config {
        delay: Duration::ZERO,
        timeout: Duration::from_secs(2),
        ..Config::default()
    };
    config.retry.attempts = 1;
    Resolver::new(config, base).unwrap()
}

#[test]
fn handles_with_or_without_at_use_one_get_and_record_the_profile() {
    for handle in ["example", "@example"] {
        for slug in ["different-slug", ""] {
            let profile = json!({"id": 42, "slug": slug, "handle": "example", "extra": null});
            let (base, server) = common::server(vec![(200, String::new(), profile.to_string())]);
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("lookup.warc");
            let actual = resolver(&base)
                .lookup_handle(handle, Some(&path))
                .unwrap()
                .unwrap();
            assert_eq!(serde_json::to_value(actual).unwrap(), profile);
            let requests = server.join().unwrap();
            assert_eq!(requests.len(), 1);
            assert!(
                requests[0].starts_with("GET /api/v1/user/example/public_profile HTTP/1.1\r\n")
            );
            let records = WarcReader::from_path(path)
                .unwrap()
                .iter_records::<NoExtension>()
                .records()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let recorded: Vec<_> = records
                .iter()
                .filter_map(|record| match record {
                    Record::Request { body, .. } => Some(body.as_slice()),
                    _ => None,
                })
                .collect();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0], requests[0].as_bytes());
        }
    }
}

#[test]
fn invalid_handles_are_rejected_before_recording_or_network_access() {
    let mut resolver = resolver("http://127.0.0.1:1/");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("must-not-exist.warc");
    for handle in [
        "",
        "@",
        "@@example",
        ".",
        "..",
        "@..",
        "a/b",
        "a?b",
        "a#b",
        "a%2fb",
        " a",
        "a ",
        "https://substack.com/@a",
    ] {
        let error = resolver.lookup_handle(handle, Some(&path)).unwrap_err();
        assert!(
            error.to_string().contains("expected a handle"),
            "{handle}: {error}"
        );
        assert!(!path.exists());
    }
}

#[test]
fn missing_profiles_are_distinguished_from_invalid_responses() {
    for (status, body) in [
        (404, ""),
        (200, "not JSON"),
        (200, r#"{"id":42}"#),
        (204, ""),
        (301, ""),
        (403, ""),
        (500, ""),
    ] {
        let (base, server) =
            common::server(vec![(status, "Location: /@other\r\n".into(), body.into())]);
        let result = resolver(&base).lookup_handle("example", None);
        if status == 404 {
            assert!(result.unwrap().is_none());
        } else {
            assert!(result.is_err(), "{status}: {body}");
        }
        assert_eq!(server.join().unwrap().len(), 1);
    }
}
