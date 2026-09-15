use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::time::Instant;

use archivindex_warc::io::write::{Compression, WarcWriter};
use archivindex_warc::record::Record;
use archivindex_warc::record::extension::NoExtension;
use archivindex_warc::value::MediaType;
use chrono::Utc;
use reqwest::StatusCode;
use reqwest::blocking::Request;
use reqwest::header::{ACCEPT, ACCEPT_ENCODING, COOKIE, HOST, HeaderMap, USER_AGENT};
use serde_json::Value;
use url::{Position, Url};

use super::{Downloader, Endpoint, Error, SOFTWARE};

#[expect(
    clippy::redundant_pub_crate,
    reason = "Limit access to the parent module."
)]
pub(super) struct Session<'a> {
    downloader: &'a Downloader,
    writer: WarcWriter<File>,
    warcinfo: Record,
    previous: Option<Instant>,
}

impl<'a> Session<'a> {
    pub(super) fn new(downloader: &'a Downloader, output: &Path) -> Result<Self, Error> {
        let warcinfo: Record = Record::warcinfo(Utc::now())
            .filename(
                output
                    .file_name()
                    .ok_or("output needs a filename")?
                    .to_str()
                    .ok_or("output filename is not UTF-8")?,
            )?
            .software(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))?
            .http_header_user_agent(SOFTWARE)?
            .build();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?;
        let mut writer = WarcWriter::new(file).with_compression(if downloader.config.gzip {
            Compression::gzip()
        } else {
            Compression::NONE
        });
        writer.write(&warcinfo.clone().into_raw()?)?;
        writer.flush()?;
        Ok(Self {
            downloader,
            writer,
            warcinfo,
            previous: None,
        })
    }

    pub(super) fn fetch(&mut self, url: &Url) -> Result<Option<Value>, Error> {
        let endpoint = Endpoint::from_url(url).ok_or("unsupported API URL")?;
        if url.origin() != self.downloader.base.origin() {
            return Err("request URL must belong to the configured publication origin".into());
        }
        if let Some(start) = self.previous {
            std::thread::sleep(self.downloader.config.delay.saturating_sub(start.elapsed()));
        }
        let mut request = self
            .downloader
            .client
            .get(url.clone())
            .header(ACCEPT, "application/json")
            .header(ACCEPT_ENCODING, "identity")
            .header(USER_AGENT, SOFTWARE)
            .header(HOST, &url[Position::BeforeHost..Position::AfterPort]);
        if let Some(value) = &self.downloader.cookie {
            request = request.header(COOKIE, value.clone());
        }
        let request = request.build()?;
        let at = Utc::now();
        let warcinfo_id = self.warcinfo.core().record_id.clone();
        let request_record: Record = Record::request(url.as_str(), at)?
            .warcinfo_id(warcinfo_id.clone())
            .body(request_message(&request)?)?;
        let request_id = request_record.core().record_id.clone();
        let target_uri = match &request_record {
            Record::Request { header, .. } => header.target_uri.clone(),
            _ => unreachable!(),
        };
        self.writer.write(&request_record.into_raw()?)?;
        self.writer.flush()?;
        let start = Instant::now();
        self.previous = Some(start);
        let capture = (|| -> Result<_, Error> {
            let response = self.downloader.client.execute(request)?;
            let status = response.status();
            let headers = response.headers().clone();
            let mut body = Vec::new();
            response
                .take(self.downloader.config.max_body_bytes + 1)
                .read_to_end(&mut body)?;
            if body.len() as u64 > self.downloader.config.max_body_bytes {
                return Err(format!("response exceeds body limit for {url}").into());
            }
            let message = response_message(status, &headers, &body)?;
            let record: Record = Record::response(url.as_str(), at)?
                .warcinfo_id(warcinfo_id.clone())
                .concurrent_to(request_id.clone())
                .content_type(MediaType::HTTP_RESPONSE)
                .body(message)?;
            Ok((record, status, headers, body))
        })();
        let metadata = Record::<NoExtension>::metadata(at)
            .target_uri(target_uri)
            .warcinfo_id(warcinfo_id)
            .concurrent_to(request_id)
            .fetch_time_ms(start.elapsed());
        let (record, status, headers, body) = match capture {
            Ok(capture) => capture,
            Err(error) => {
                self.writer.write(&metadata.build().into_raw()?)?;
                self.writer.flush()?;
                return Err(error);
            }
        };
        let metadata = metadata
            .concurrent_to(record.core().record_id.clone())
            .build();
        self.writer.write(&record.into_raw()?)?;
        self.writer.write(&metadata.into_raw()?)?;
        self.writer.flush()?;
        if status.as_u16() == 200 {
            let data: Value = serde_json::from_slice(&body)
                .map_err(|e| format!("invalid JSON for {url}: {e}; response retained"))?;
            endpoint
                .validate(&data)
                .map_err(|e| format!("invalid response for {url}: {e}; response retained"))?;
            Ok(Some(data))
        } else if status.as_u16() == 404 && matches!(endpoint, Endpoint::Post(_)) {
            Ok(None)
        } else {
            let retry = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unspecified");
            let location = headers
                .get("location")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("unspecified");
            Err(format!("HTTP {status} for {url}; Retry-After: {retry}; Location: {location}; completed records retained").into())
        }
    }
}

fn request_message(request: &Request) -> Result<Vec<u8>, Error> {
    let mut message = format!(
        "GET {} HTTP/1.1\r\n",
        &request.url()[Position::BeforePath..Position::AfterQuery]
    )
    .into_bytes();
    for (name, value) in request.headers() {
        write!(&mut message, "{name}: ")?;
        message.extend_from_slice(value.as_bytes());
        message.extend_from_slice(b"\r\n");
    }
    message.extend_from_slice(b"\r\n");
    Ok(message)
}

fn response_message(
    status: StatusCode,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<Vec<u8>, Error> {
    // Reqwest decodes transfer framing; retain entity bytes and normalize framing headers.
    let mut message = format!(
        "HTTP/1.1 {} {}\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    )
    .into_bytes();
    for (name, value) in headers {
        if matches!(
            name.as_str(),
            "transfer-encoding" | "content-length" | "connection" | "trailer"
        ) {
            continue;
        }
        write!(&mut message, "{name}: ")?;
        message.extend_from_slice(value.as_bytes());
        message.extend_from_slice(b"\r\n");
    }
    write!(&mut message, "content-length: {}\r\n\r\n", body.len())?;
    message.extend_from_slice(body);
    Ok(message)
}
