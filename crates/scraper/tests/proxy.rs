//! SOCKS routing and public IP checks share the transport used for all Substack requests.

mod common;

use std::io::{Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::time::Duration;

use archivindex_substack_scraper::resolve::{Config, Resolver};
use archivindex_substack_scraper::{Config as PublicationConfig, Downloader, ResumeState};
use archivindex_warc::io::read::WarcReader;
use archivindex_warc::record::Record;
use archivindex_warc::record::extension::NoExtension;

fn config(proxy: Option<String>) -> Config {
    let mut config = Config {
        proxy,
        delay: Duration::ZERO,
        timeout: Duration::from_secs(2),
        ..Config::default()
    };
    config.retry.attempts = 1;
    config
}

fn byte(stream: &mut TcpStream) -> u8 {
    let mut byte = [0];
    stream.read_exact(&mut byte).unwrap();
    byte[0]
}

fn field(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = vec![0; usize::from(byte(stream))];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}

fn authenticated_tunnel(stream: &mut TcpStream, host: &str) {
    assert_eq!(byte(stream), 5);
    assert!(field(stream).contains(&2));
    stream.write_all(&[5, 2]).unwrap();
    assert_eq!(byte(stream), 1);
    assert_eq!(field(stream), b"username");
    assert_eq!(field(stream), b"password");
    stream.write_all(&[1, 0]).unwrap();
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(header, [5, 1, 0, 3]);
    assert_eq!(field(stream), host.as_bytes());
    assert_eq!([byte(stream), byte(stream)], 80u16.to_be_bytes());
    stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
}

#[test]
fn remote_dns_proxy_carries_ip_check_head_get_and_retries_without_leaking_credentials() {
    let mut targets = [
        "ipify.invalid",
        "profile.invalid",
        "profile.invalid",
        "profile.invalid",
    ]
    .into_iter();
    let (base, server) = common::server_with_connection(
        |_| {
            vec![
                (200, String::new(), "203.0.113.42\n".into()),
                (429, "Retry-After: 0\r\n".into(), String::new()),
                (301, "Location: /@example\r\n".into(), String::new()),
                (200, String::new(), r#"{"id":42,"slug":"example"}"#.into()),
            ]
        },
        move |stream| {
            authenticated_tunnel(stream, targets.next().unwrap());
        },
    );
    let proxy = base.replace("http://", "socks5h://username:password@");
    let mut config = config(Some(proxy));
    config.retry.attempts = 2;
    let mut resolver = Resolver::new(config, "http://profile.invalid/").unwrap();
    assert_eq!(
        resolver
            .public_ip("http://ipify.invalid/")
            .unwrap()
            .to_string(),
        "203.0.113.42"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profile.warc");
    assert!(resolver.resolve_user(42, Some(&path)).unwrap().is_some());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[0].starts_with("GET / HTTP/1.1\r\n"));
    assert!(requests[1].starts_with("HEAD /profile/42 HTTP/1.1\r\n"));
    assert!(requests[3].starts_with("GET /api/v1/user/example/public_profile HTTP/1.1\r\n"));
    for request in &requests {
        assert!(!request.contains("username"));
        assert!(!request.contains("password"));
    }
    let bytes = std::fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("username"));
    assert!(!text.contains("password"));
    assert!(text.contains("archivindex-proxy: socks5h://127.0.0.1:"));
    let records = WarcReader::from_path(path)
        .unwrap()
        .iter_records::<NoExtension>()
        .records()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(record, Record::Request { .. }))
            .count(),
        3
    );
}

#[test]
fn socks5_resolves_destination_names_locally() {
    let (base, server) = common::server_with_connection(
        |_| vec![(200, String::new(), "2001:db8::1\n".into())],
        |stream| {
            assert_eq!(byte(stream), 5);
            assert!(field(stream).contains(&0));
            stream.write_all(&[5, 0]).unwrap();
            assert_eq!([byte(stream), byte(stream), byte(stream)], [5, 1, 0]);
            let address: IpAddr = match byte(stream) {
                1 => {
                    let mut bytes = [0; 4];
                    stream.read_exact(&mut bytes).unwrap();
                    bytes.into()
                }
                4 => {
                    let mut bytes = [0; 16];
                    stream.read_exact(&mut bytes).unwrap();
                    bytes.into()
                }
                kind => panic!("expected a locally resolved IP address, got SOCKS type {kind}"),
            };
            assert!(address.is_loopback());
            assert_eq!([byte(stream), byte(stream)], 80u16.to_be_bytes());
            stream.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
        },
    );
    let mut resolver = Resolver::new(
        config(Some(base.replace("http://", "socks5://"))),
        "http://localhost/",
    )
    .unwrap();
    assert_eq!(
        resolver.public_ip("http://localhost/").unwrap().to_string(),
        "2001:db8::1"
    );
    assert_eq!(server.join().unwrap().len(), 1);
}

#[test]
fn public_ip_rejects_bad_statuses_invalid_addresses_and_truncation() {
    for (status, body) in [
        (302, "203.0.113.1".into()),
        (503, "203.0.113.1".into()),
        (200, "not an IP address".into()),
        (200, format!("203.0.113.1{}", " ".repeat(8192))),
    ] {
        let (base, server) = common::server(vec![(status, "Location: /redirect\r\n".into(), body)]);
        let mut resolver = Resolver::new(config(None), &base).unwrap();
        assert!(resolver.public_ip(&base).is_err());
        assert_eq!(server.join().unwrap().len(), 1);
    }
}

#[test]
fn proxy_failures_never_fall_back_to_a_reachable_origin() {
    let origin = TcpListener::bind("127.0.0.1:0").unwrap();
    origin.set_nonblocking(true).unwrap();
    let base = format!("http://{}/", origin.local_addr().unwrap());
    let unavailable = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = format!("socks5h://{}", unavailable.local_addr().unwrap());
    drop(unavailable);
    let downloader = Downloader::new(
        PublicationConfig {
            proxy: Some(proxy.clone()),
            ..Default::default()
        },
        &base,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    assert!(downloader.public_ip(&base).is_err());
    assert!(
        downloader
            .archive(&ResumeState::default(), dir.path().join("failed.warc"))
            .is_err()
    );
    let mut resolver = Resolver::new(config(Some(proxy)), &base).unwrap();
    assert!(resolver.public_ip(&base).is_err());
    assert!(resolver.resolve_user(42, None).is_err());
    assert_eq!(
        origin.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn publication_proxy_routes_every_request_and_excludes_cookies_from_the_ip_check() {
    let mut targets = [
        "ipify.invalid",
        "publication.invalid",
        "publication.invalid",
        "publication.invalid",
    ]
    .into_iter();
    let post = r#"{"id":1,"publication_id":1,"slug":"one","title":"One"}"#;
    let (base, server) = common::server_with_connection(
        |_| {
            vec![
                (200, String::new(), "203.0.113.42\n".into()),
                (200, String::new(), format!("[{post}]")),
                (200, String::new(), post.into()),
                (200, String::new(), "[]".into()),
            ]
        },
        move |stream| {
            authenticated_tunnel(stream, targets.next().unwrap());
        },
    );
    let downloader = Downloader::new(
        PublicationConfig {
            proxy: Some(base.replace("http://", "socks5h://username:password@")),
            cookie: Some("substack.sid=cookie-secret".into()),
            delay: Duration::ZERO,
            ..Default::default()
        },
        "http://publication.invalid/",
    )
    .unwrap();
    assert_eq!(
        downloader
            .public_ip("http://ipify.invalid/")
            .unwrap()
            .to_string(),
        "203.0.113.42"
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("publication.warc");
    assert_eq!(
        downloader.archive(&ResumeState::default(), &path).unwrap(),
        1
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(!requests[0].to_ascii_lowercase().contains("cookie"));
    for request in &requests[1..] {
        assert!(request.contains("substack.sid=cookie-secret"));
    }
    let warc = std::fs::read_to_string(path).unwrap();
    assert!(!warc.contains("ipify.invalid"));
    assert!(!warc.contains("username"));
    assert!(!warc.contains("password"));
    assert!(warc.contains("substack.sid=cookie-secret"));
}
