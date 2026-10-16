use std::net::IpAddr;
use std::path::Path;
use std::time::{Duration, Instant};

use archivindex_archiver::capture::{ProgressControl, ProgressEvent};
use archivindex_archiver::config::{SessionConfig, Software};
use archivindex_archiver::session::{Capture, Driver, Inspection, Request, RetryConfig, Session};
use archivindex_archiver::{Archiver, Config};
use url::Url;

use super::{Error, SOFTWARE};

#[expect(
    clippy::redundant_pub_crate,
    reason = "Limit access to the parent module."
)]
pub(super) struct Transport {
    config: Config,
    delay: Duration,
    previous: Option<Instant>,
}

impl Transport {
    pub(super) fn new(
        proxy: Option<String>,
        delay: Duration,
        timeout: Duration,
        max_response_bytes: u64,
        retry: RetryConfig,
        gzip: bool,
    ) -> Result<Self, Error> {
        if timeout.is_zero() || max_response_bytes == 0 {
            return Err("timeout and response limit must be positive".into());
        }
        Ok(Self {
            delay,
            previous: None,
            config: Config {
                proxy,
                user_agent: SOFTWARE.into(),
                software: Software {
                    name: env!("CARGO_PKG_NAME").into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                max_redirects: 0,
                timeout,
                max_capture_time: Some(timeout),
                max_response_length: Some(max_response_bytes),
                gzip_warc: gzip,
                // Keep each response self-contained, including repeated retry payloads.
                min_revisit_payload_length: u64::MAX,
                session: SessionConfig {
                    request_delay: Duration::ZERO,
                    retry,
                    revisit_index: None,
                },
                ..Config::default()
            },
        })
    }

    pub(super) fn public_ip(&mut self, endpoint: &str) -> Result<IpAddr, Error> {
        let mut lookup = IpLookup {
            pending: Some(Request::seed(endpoint)),
            address: None,
        };
        let mut config = self.config.clone();
        config.session.retry.attempts = 1;
        config.max_response_length = Some(config.max_response_length.unwrap_or(4096).min(4096));
        self.run("public-ip", &mut lookup, None, config)?;
        lookup
            .address
            .ok_or_else(|| "public IP lookup returned no address".into())
    }

    pub(super) fn capture(
        &mut self,
        id: &str,
        driver: &mut impl Driver,
        output: Option<&Path>,
    ) -> Result<(), Error> {
        self.run(id, driver, output, self.config.clone())
    }

    fn run(
        &mut self,
        id: &str,
        driver: &mut impl Driver,
        output: Option<&Path>,
        mut config: Config,
    ) -> Result<(), Error> {
        let temporary = output.is_none().then(tempfile::tempdir).transpose()?;
        let path = output.map_or_else(
            || {
                temporary
                    .as_ref()
                    .expect("temporary directory exists")
                    .path()
                    .join("lookup.warc")
            },
            Path::to_path_buf,
        );
        config.gzip_warc |= path.extension().is_some_and(|extension| extension == "gz");
        let summary = Session::new(Archiver::new(config)?, id, driver, path)?
            .progress(|event: ProgressEvent<'_>| {
                if matches!(event, ProgressEvent::Started { .. }) {
                    if let Some(previous) = self.previous {
                        std::thread::sleep(self.delay.saturating_sub(previous.elapsed()));
                    }
                    self.previous = Some(Instant::now());
                }
                ProgressControl::Continue
            })
            .run()?;
        if let Some(error) = summary.fatal_error {
            return Err(error.into());
        }
        if let Some(failure) = summary.failures.into_iter().next() {
            return Err(failure.error.into());
        }
        if summary.cancelled
            || summary
                .seed_captures
                .iter()
                .chain(&summary.extra_captures)
                .any(|capture| capture.truncated.is_some())
        {
            return Err("capture was incomplete".into());
        }
        Ok(())
    }
}

#[expect(
    clippy::redundant_pub_crate,
    reason = "Limit access to the parent module."
)]
pub(super) fn json_request(url: &Url, via: Option<&str>) -> Result<Request, Error> {
    let mut request = via.map_or_else(
        || Request::seed(url.as_str()),
        |via| Request::extra(url.as_str(), via),
    );
    request
        .headers
        .insert("accept", "application/json".parse()?);
    request
        .headers
        .insert("accept-encoding", "identity".parse()?);
    Ok(request)
}

struct IpLookup {
    pending: Option<Request>,
    address: Option<IpAddr>,
}

impl Driver for IpLookup {
    fn next(&mut self) -> Option<Request> {
        self.pending.take()
    }

    fn inspect(&mut self, capture: &Capture<'_>) -> Inspection {
        if capture.status == 200 {
            match std::str::from_utf8(capture.payload)
                .ok()
                .and_then(|body| body.trim().parse().ok())
            {
                Some(address) => {
                    self.address = Some(address);
                    Inspection::default()
                }
                None => Inspection::error("public IP lookup returned an invalid IP address"),
            }
        } else {
            Inspection::error(format!("public IP lookup returned HTTP {}", capture.status))
        }
    }
}
