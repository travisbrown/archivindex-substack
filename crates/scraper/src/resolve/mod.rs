//! Resolve user IDs and handles through recorded HEAD redirects and public profile responses.

mod lookup;

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use archivindex_archiver::session::{Request, RetryConfig};
use archivindex_substack_model::profile::PublicProfile;
use reqwest::Method;
use url::Url;

use crate::Error;
use crate::transport::{Transport, json_request};

/// Whether an existing user has a handle. A missing user is represented separately by `None`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Handle {
    /// The handle without its leading `@`.
    Set(String),
    /// The numeric profile URL returned 200 without a redirect.
    Unset,
}

/// Settings for profile resolution through an archiver session.
pub struct Config {
    /// SOCKS5 proxy URL. Use `socks5h://` for remote DNS. Environment proxy settings are ignored.
    pub proxy: Option<String>,
    /// Minimum interval between request starts, including retries and later lookups (1.5 seconds).
    pub delay: Duration,
    /// Socket idle timeout and total capture time limit, excluding DNS (default 30 seconds).
    pub timeout: Duration,
    /// Maximum captured response size, including headers (default 16 MiB).
    pub max_response_bytes: u64,
    /// Retry policy for transient failures and retryable HTTP statuses.
    pub retry: RetryConfig,
    /// Compress WARC records. A `.gz` output suffix also enables compression.
    pub gzip: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            proxy: None,
            delay: Duration::from_millis(1500),
            timeout: Duration::from_secs(30),
            max_response_bytes: 16 * 1024 * 1024,
            retry: RetryConfig {
                attempts: 6,
                initial_backoff: Duration::from_secs(5),
                max_backoff: Duration::from_secs(300),
            },
            gzip: false,
        }
    }
}

/// A synchronous public-profile resolver. Redirects are captured but never followed.
///
/// Each lookup uses an `archivindex-archiver` session. Supply an output path to retain its WARC,
/// including the HEAD exchange, profile GET, and retries. Without a path, the session uses a
/// temporary archive that is removed when the lookup returns. No login cookies are supplied.
pub struct Resolver {
    transport: Transport,
    base: Url,
}

impl Resolver {
    /// Configure resolution against `https://substack.com/` or a local HTTP(S) test origin.
    ///
    /// The base URL must contain no credentials, path beyond `/`, query, or fragment.
    pub fn new(config: Config, base_url: &str) -> Result<Self, Error> {
        let base = crate::validate_origin(Url::parse(base_url)?)?;
        Ok(Self {
            base,
            transport: Transport::new(
                config.proxy,
                config.delay,
                config.timeout,
                config.max_response_bytes,
                config.retry,
                config.gzip,
            )?,
        })
    }

    /// Resolve a handle with `HEAD /profile/{id}`.
    ///
    /// Returns `None` for 404 and `Some(Handle::Unset)` for 200. Only a redirect to a single
    /// `/@handle` path on the configured origin yields a handle. Other responses are errors.
    /// An optional WARC output path must not already exist.
    pub fn resolve_handle(
        &mut self,
        user_id: u64,
        output: Option<&Path>,
    ) -> Result<Option<Handle>, Error> {
        Ok(self.run(user_id, false, output)?.handle)
    }

    /// Resolve the public profile after looking up the handle.
    ///
    /// Returns `None` for a missing user, an unset handle, or a profile GET returning 404.
    /// Malformed data, a different user ID, and incomplete captures are errors.
    /// An optional WARC output path retains both requests and must not already exist.
    pub fn resolve_user(
        &mut self,
        user_id: u64,
        output: Option<&Path>,
    ) -> Result<Option<PublicProfile>, Error> {
        Ok(self.run(user_id, true, output)?.profile)
    }

    /// Resolve the profile slug, which can differ from the handle.
    ///
    /// Uses the same requests and recording behavior as [`Self::resolve_user`]. An absent profile
    /// or empty slug returns `None`. Profile slugs do not use the post-slug validation rules.
    pub fn resolve_slug(
        &mut self,
        user_id: u64,
        output: Option<&Path>,
    ) -> Result<Option<String>, Error> {
        Ok(self
            .resolve_user(user_id, output)?
            .map(|profile| profile.slug)
            .filter(|slug| !slug.is_empty()))
    }

    /// Look up a public profile directly by handle, with or without one leading `@`.
    ///
    /// Makes a profile GET without a HEAD lookup. Returns `None` for 404; invalid handles,
    /// unexpected statuses, malformed profiles, and incomplete captures are errors.
    /// An optional WARC output path retains the GET and retries and must not already exist.
    pub fn lookup_handle(
        &mut self,
        handle: &str,
        output: Option<&Path>,
    ) -> Result<Option<PublicProfile>, Error> {
        let handle = handle.strip_prefix('@').unwrap_or(handle);
        if !valid_handle(handle) {
            return Err("expected a handle containing ASCII letters, digits, hyphens, underscores, or periods, optionally preceded by @; . and .. are not handles".into());
        }
        let mut lookup = Lookup {
            user_id: None,
            full: true,
            base: self.base.clone(),
            pending: Some(profile_request(&self.base, handle, None)?),
            handle: Some(Handle::Set(handle.into())),
            profile: None,
        };
        self.transport
            .capture("lookup-handle", &mut lookup, output)?;
        Ok(lookup.profile)
    }

    /// Look up the public IP through the same proxy and transport as profile resolution.
    ///
    /// The endpoint must return a plain IPv4 or IPv6 address with status 200. Redirects, truncated
    /// responses, and invalid addresses are errors. The check makes one attempt and retains no WARC.
    /// For example, pass `https://api.ipify.org/` before beginning a set of lookups.
    pub fn public_ip(&mut self, endpoint: &str) -> Result<IpAddr, Error> {
        self.transport.public_ip(endpoint)
    }

    fn run(&mut self, user_id: u64, full: bool, output: Option<&Path>) -> Result<Lookup, Error> {
        let url = self.base.join(&format!("profile/{user_id}"))?;
        let mut lookup = Lookup {
            user_id: Some(user_id),
            full,
            base: self.base.clone(),
            pending: Some(Request::seed(url.as_str()).with_method(Method::HEAD)),
            handle: None,
            profile: None,
        };
        self.transport
            .capture(&format!("resolve-{user_id}"), &mut lookup, output)?;
        Ok(lookup)
    }
}

struct Lookup {
    user_id: Option<u64>,
    full: bool,
    base: Url,
    pending: Option<Request>,
    handle: Option<Handle>,
    profile: Option<PublicProfile>,
}

fn valid_handle(handle: &str) -> bool {
    !handle.is_empty()
        && !matches!(handle, "." | "..")
        && handle
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn profile_request(base: &Url, handle: &str, via: Option<&str>) -> Result<Request, Error> {
    let url = base.join(&format!("api/v1/user/{handle}/public_profile"))?;
    json_request(&url, via)
}
