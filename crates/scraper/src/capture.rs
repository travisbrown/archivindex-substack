use std::collections::{BTreeSet, VecDeque};
use std::path::Path;

use archivindex_archiver::capture::CaptureSummary;
use archivindex_archiver::session::{Capture, Driver, Inspection, Request};
use archivindex_substack_model::Post;
use serde_json::Value;
use url::Url;

use super::{Downloader, Endpoint, Error, ResumeState, transport};

impl Downloader {
    /// Archive all discoverable posts, skipping successful post lookups from previous segments.
    ///
    /// Output is created exclusively. Valid post responses and post 404s count as completed.
    /// Other HTTP statuses, malformed data, and transport failures stop the run. Earlier records
    /// remain readable. A configured post limit creates a partial segment suitable for resume.
    /// The archiver flushes each exchange to a `.partial` file and publishes it when the run ends.
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
        let mut driver = Publication {
            downloader: self,
            pending: VecDeque::new(),
            completed: resume.completed.clone(),
            listed: BTreeSet::new(),
            offset: 0,
            count: 0,
            pending_post: None,
            progress: &mut progress,
        };
        // Earlier listings may contain posts that have since disappeared from the current archive.
        for url in &resume.discovered {
            let url = Url::parse(url)?;
            if url.origin() == self.base.origin() && !driver.completed.contains(url.as_str()) {
                driver.pending.push_back(driver.request(&url, None)?);
            }
        }
        driver.queue_page()?;
        (driver.progress)(0);
        self.transport()?
            .capture("publication", &mut driver, Some(output.as_ref()))?;
        Ok(driver.count)
    }
}

struct Publication<'a, F> {
    downloader: &'a Downloader,
    pending: VecDeque<Request>,
    completed: BTreeSet<String>,
    listed: BTreeSet<u64>,
    offset: usize,
    count: usize,
    pending_post: Option<String>,
    progress: F,
}

impl<F: FnMut(usize)> Publication<'_, F> {
    fn request(&self, url: &Url, via: Option<&str>) -> Result<Request, Error> {
        if url.origin() != self.downloader.base.origin() || Endpoint::from_url(url).is_none() {
            return Err("request must be a supported API URL on the publication origin".into());
        }
        let mut request = transport::json_request(url, via)?;
        if let Some(cookie) = &self.downloader.cookie {
            request.headers.insert("cookie", cookie.clone());
        }
        Ok(request)
    }

    fn queue_page(&mut self) -> Result<(), Error> {
        let mut url = self.downloader.base.join("api/v1/archive")?;
        url.query_pairs_mut()
            .append_pair("sort", "new")
            .append_pair("offset", &self.offset.to_string())
            .append_pair("limit", &self.downloader.config.page_size.to_string());
        self.pending.push_back(self.request(&url, None)?);
        Ok(())
    }

    fn inspect_response(&mut self, capture: &Capture<'_>) -> Result<(), Error> {
        let endpoint =
            Endpoint::from_url(&Url::parse(capture.url)?).ok_or("unsupported API URL")?;
        if capture.status == 404 && matches!(endpoint, Endpoint::Post(_)) {
            self.pending_post = Some(capture.url.into());
            return Ok(());
        }
        if capture.status != 200 {
            return Err(format!(
                "HTTP {} for {}; Retry-After: {}; Location: {}; completed records retained",
                capture.status,
                capture.url,
                capture.header("retry-after").unwrap_or("unspecified"),
                capture.header("location").unwrap_or("unspecified"),
            )
            .into());
        }
        let data: Value = serde_json::from_slice(capture.payload)?;
        endpoint.validate(&data)?;
        match endpoint {
            Endpoint::Post(_) => self.pending_post = Some(capture.url.into()),
            Endpoint::Archive => {
                let posts: Vec<Post> = serde_json::from_value(data)?;
                if posts.is_empty() {
                    return Ok(());
                }
                let mut new_ids = 0;
                for post in &posts {
                    if self.listed.insert(post.id) {
                        new_ids += 1;
                        let url = self
                            .downloader
                            .base
                            .join(&format!("api/v1/posts/{}", post.slug))?;
                        if !self.completed.contains(url.as_str()) {
                            self.pending
                                .push_back(self.request(&url, Some(capture.url))?);
                        }
                    }
                }
                if new_ids == 0 {
                    return Err("archive pagination made no progress; repeated post IDs; completed records retained".into());
                }
                // Short pages do not end pagination: the service can cap the requested limit.
                self.offset = self
                    .offset
                    .checked_add(posts.len())
                    .ok_or("archive offset overflow")?;
                self.queue_page()?;
            }
        }
        Ok(())
    }
}

impl<F: FnMut(usize)> Driver for Publication<'_, F> {
    fn next(&mut self) -> Option<Request> {
        if self
            .downloader
            .config
            .max_posts
            .is_some_and(|limit| self.count >= limit)
        {
            None
        } else {
            // Different post IDs can share a slug. Skip a URL completed earlier in this page.
            std::iter::from_fn(|| self.pending.pop_front())
                .find(|request| !self.completed.contains(&request.url))
        }
    }

    fn inspect(&mut self, capture: &Capture<'_>) -> Inspection {
        match self.inspect_response(capture) {
            Ok(()) => Inspection::default(),
            Err(error) => Inspection::error(error.to_string()),
        }
    }

    fn recorded(&mut self, capture: Option<&CaptureSummary>) {
        if capture.is_none_or(|capture| capture.truncated.is_some()) {
            self.pending.clear();
            self.pending_post = None;
        } else if let Some(url) = self.pending_post.take() {
            self.completed.insert(url);
            self.count += 1;
            (self.progress)(self.count);
        }
    }

    fn failed(&mut self, _url: &str, _error: &archivindex_archiver::Error) {
        self.pending.clear();
    }
}
