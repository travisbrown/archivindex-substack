//! Download Substack publication posts and preserve HTTP exchanges in WARC archives.
//!
//! Capture is sequential. Every exchange is flushed before validation or the next request.
//! Resume retries previously discovered posts and walks current listings from offset zero.

mod capture;
mod read;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use archivindex_substack_model::{Post, Slug};
use reqwest::blocking::Client;
use reqwest::header::HeaderValue;
use serde::Serialize;
use serde_json::Value;
use url::Url;

/// Errors from HTTP, WARC, JSON, configuration, or local I/O operations.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

const SOFTWARE: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// Settings for a sequential publication capture.
pub struct Config {
    /// Optional Cookie header for this publication, also preserved in WARC request records.
    pub cookie: Option<String>,
    /// Minimum interval between request starts (default two seconds, not an API quota guarantee).
    pub delay: Duration,
    /// Timeout for each request, including its response body.
    pub timeout: Duration,
    /// Maximum response body size in bytes.
    pub max_body_bytes: u64,
    /// Requested number of summaries per archive page, from 1 through 100.
    pub page_size: usize,
    /// Stop successfully after this many post lookups in this segment; listings do not count.
    pub max_posts: Option<usize>,
    /// Compress each WARC record in its own gzip member.
    pub gzip: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cookie: None,
            delay: Duration::from_secs(2),
            timeout: Duration::from_secs(60),
            max_body_bytes: 16 * 1024 * 1024,
            page_size: 20,
            max_posts: None,
            gzip: false,
        }
    }
}

/// A downloader bound to one publication origin. Redirects are archived without being followed.
pub struct Downloader {
    client: Client,
    config: Config,
    base: Url,
    cookie: Option<HeaderValue>,
}

impl Downloader {
    /// Configure a publication by subdomain slug or an HTTP(S) origin, including custom domains.
    ///
    /// URLs must have no path beyond `/`, credentials, query, or fragment. Cookies, when supplied,
    /// are sent only to this origin. HTTP support is intended for local servers.
    pub fn new(config: Config, publication: &str) -> Result<Self, Error> {
        let base = publication_url(publication)?;
        if config.timeout.is_zero()
            || config.max_body_bytes == 0
            || config.max_body_bytes == u64::MAX
            || !(1..=100).contains(&config.page_size)
            || config.max_posts == Some(0)
        {
            return Err("timeout, body limit, and max-posts must be positive; body limit must be below u64::MAX; page size must be 1..=100".into());
        }
        let cookie = config
            .cookie
            .as_ref()
            .map(|cookie| {
                if cookie.trim().is_empty() {
                    return Err("Cookie header is empty".into());
                }
                let mut value = HeaderValue::from_str(cookie).map_err(|_| -> Error {
                    "Cookie header is not a valid HTTP header value".into()
                })?;
                value.set_sensitive(true);
                Ok::<_, Error>(value)
            })
            .transpose()?;
        let client = Client::builder()
            .user_agent(SOFTWARE)
            .http1_only()
            .redirect(reqwest::redirect::Policy::none())
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .timeout(config.timeout)
            .build()?;
        Ok(Self {
            client,
            config,
            base,
            cookie,
        })
    }

    /// Archive all discoverable posts, skipping successful post lookups from previous segments.
    ///
    /// Output is created exclusively. Valid post responses and post 404s count as completed.
    /// Other HTTP statuses, malformed data, and transport failures stop the run. Earlier records
    /// remain readable. A configured post limit creates a partial segment suitable for resume.
    pub fn archive(&self, resume: &ResumeState, output: impl AsRef<Path>) -> Result<usize, Error> {
        self.archive_with_progress(resume, output, |_| {})
    }

    /// Archive with a notification after each completed post lookup has been flushed.
    ///
    /// The first notification is zero. Listings and skipped posts do not advance the count.
    pub fn archive_with_progress(
        &self,
        resume: &ResumeState,
        output: impl AsRef<Path>,
        mut progress: impl FnMut(usize),
    ) -> Result<usize, Error> {
        let mut session = capture::Session::new(self, output.as_ref())?;
        progress(0);
        let mut completed = resume.completed.clone();
        let mut count = 0;
        // Earlier listings may contain posts that have since disappeared from the current archive.
        for url in &resume.discovered {
            let url = Url::parse(url)?;
            if url.origin() == self.base.origin() && !completed.contains(url.as_str()) {
                session.fetch(&url)?;
                completed.insert(url.to_string());
                count += 1;
                progress(count);
                if self.config.max_posts.is_some_and(|limit| count >= limit) {
                    return Ok(count);
                }
            }
        }
        let mut offset = 0usize;
        let mut listed = BTreeSet::new();
        loop {
            let mut url = self.base.join("api/v1/archive")?;
            url.query_pairs_mut()
                .append_pair("sort", "new")
                .append_pair("offset", &offset.to_string())
                .append_pair("limit", &self.config.page_size.to_string());
            let value = session
                .fetch(&url)?
                .ok_or("archive listing returned no data")?;
            let posts: Vec<Post> = serde_json::from_value(value)?;
            if posts.is_empty() {
                return Ok(count);
            }
            let mut new_ids = 0;
            for post in &posts {
                if !listed.insert(post.id) {
                    continue;
                }
                new_ids += 1;
                let url = self.base.join(&format!("api/v1/posts/{}", post.slug))?;
                if !completed.contains(url.as_str()) {
                    session.fetch(&url)?;
                    completed.insert(url.to_string());
                    count += 1;
                    progress(count);
                    if self.config.max_posts.is_some_and(|limit| count >= limit) {
                        return Ok(count);
                    }
                }
            }
            if new_ids == 0 {
                return Err("archive pagination made no progress; repeated post IDs; completed records retained".into());
            }
            // Continue even after a short page: the service can cap the requested limit.
            offset = offset
                .checked_add(posts.len())
                .ok_or("archive offset overflow")?;
        }
    }
}

/// Successful capture category.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureKind {
    /// A page of post summaries.
    Archive,
    /// A post detail response, including any content available to the requesting session.
    Post,
}

/// One valid JSON response with provenance. Repeated captures remain separate.
#[derive(Debug, Serialize)]
pub struct ArchivedCapture {
    /// API response category.
    pub kind: CaptureKind,
    /// Request URL.
    pub url: String,
    /// WARC capture date.
    pub captured_at: String,
    /// Original JSON, preserving unknown fields and explicit nulls.
    pub data: Value,
}

/// Resume evidence derived from valid archived HTTP responses.
#[derive(Debug, Default)]
pub struct ResumeState {
    completed: BTreeSet<String>,
    discovered: BTreeSet<String>,
}

impl ResumeState {
    /// Merge evidence from another segment. Use the same authentication scope across segments.
    pub fn extend(&mut self, other: Self) {
        self.completed.extend(other.completed);
        self.discovered.extend(other.discovered);
    }

    /// Number of distinct post URLs with valid 200 responses or complete 404 responses.
    #[must_use]
    pub fn completed_posts(&self) -> usize {
        self.completed.len()
    }
}

/// Parsed contents of one archive.
#[derive(Debug, Default)]
pub struct ArchiveContents {
    /// Valid listing and post captures, in record order.
    pub captures: Vec<ArchivedCapture>,
    /// Evidence for resuming into a new segment.
    pub resume: ResumeState,
    /// Counts of complete responses by HTTP status, including failures and malformed JSON.
    pub status_counts: BTreeMap<u16, usize>,
}

/// Read a plain or gzip WARC, detecting compression from the file bytes.
///
/// Reads response records at supported Substack API paths on any HTTP(S) origin. Unrelated
/// paths and truncated captures are excluded. Broken WARC framing is an error. One archive's
/// extracted JSON is buffered in memory. Failed responses remain in the WARC but are not exported.
pub fn read_archive(path: impl AsRef<Path>) -> Result<ArchiveContents, Error> {
    read::read(path.as_ref())
}

fn publication_url(input: &str) -> Result<Url, Error> {
    let input = input.trim();
    let url = if input.contains("://") {
        Url::parse(input)?
    } else {
        if input.is_empty()
            || input.len() > 63
            || input.starts_with('-')
            || input.ends_with('-')
            || !input
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(
                "expected a publication subdomain slug or a full HTTP(S) origin URL".into(),
            );
        }
        Url::parse(&format!("https://{input}.substack.com/"))?
    };
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("publication URL must be an HTTP(S) origin without a path, credentials, query, or fragment".into());
    }
    Ok(url)
}

enum Endpoint {
    Archive,
    Post(Slug),
}

impl Endpoint {
    fn from_url(url: &Url) -> Option<Self> {
        if !matches!(url.scheme(), "http" | "https")
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return None;
        }
        if url.path() == "/api/v1/archive" {
            let pairs: BTreeMap<_, _> = url.query_pairs().collect();
            if pairs.len() != 3
                || url.query_pairs().count() != 3
                || pairs.get("sort")?.as_ref() != "new"
                || pairs.get("offset")?.parse::<usize>().is_err()
                || !(1..=100).contains(&pairs.get("limit")?.parse::<usize>().ok()?)
            {
                return None;
            }
            Some(Self::Archive)
        } else if url.query().is_none() {
            Some(Self::Post(
                url.path().strip_prefix("/api/v1/posts/")?.parse().ok()?,
            ))
        } else {
            None
        }
    }

    fn validate(&self, data: &Value) -> Result<CaptureKind, Error> {
        match self {
            Self::Archive => {
                serde_json::from_value::<Vec<Post>>(data.clone())?;
                Ok(CaptureKind::Archive)
            }
            Self::Post(slug) => {
                let post: Post = serde_json::from_value(data.clone())?;
                if post.slug != *slug {
                    return Err("response post slug does not match the requested slug".into());
                }
                Ok(CaptureKind::Post)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_origins_are_normalized_and_paths_rejected() {
        assert_eq!(
            publication_url("On").unwrap().as_str(),
            "https://on.substack.com/"
        );
        assert_eq!(
            publication_url("https://custom.example").unwrap().as_str(),
            "https://custom.example/"
        );
        for input in [
            "",
            "a.b",
            "a/b",
            "-x",
            "x-",
            "https://x/p/post",
            "https://x/?a",
            "https://x/#a",
            "https://user:secret@x",
            "ftp://x",
        ] {
            assert!(publication_url(input).is_err(), "{input}");
        }
    }
}
