# archivindex-substack

Rust libraries and a command-line tool for resolving Substack users, downloading publication posts,
archiving HTTP responses in WARC files, and exporting captured post JSON for analysis. Follows the
model / scraper / CLI workspace layout of `archivindex-gravatar` and `archivindex-wordpress`, using
their shared `archivindex-warc` library.

| Crate                                                    | Purpose                                                            |
| -------------------------------------------------------- | ------------------------------------------------------------------ |
| [`archivindex-substack-model`](crates/model/)            | Post, author, and public profile models; validated post slugs      |
| [`archivindex-substack-scraper`](crates/scraper/)        | User resolution, publication capture, and WARC reading and writing |
| [`archivindex-substack-scraper-cli`](tools/scraper-cli/) | Resolution, `archive`, `export`, and `inspect` commands            |

## Getting started

Requires Rust 1.98 or later. Install the CLI:

```console
cargo install --locked --path tools/scraper-cli
archivindex-substack-scraper --help
```

Capture a publication by its Substack subdomain slug or full origin URL. Custom domains are
supported when supplied as full URLs. Use the publication's current canonical origin; redirects are
recorded and stop the run, with the destination included in the error.

```sh
mkdir -p data
archivindex-substack-scraper archive --publication on --output data/on-01.warc.gz
archivindex-substack-scraper inspect data/on-01.warc.gz
archivindex-substack-scraper export data/on-01.warc.gz > data/on.jsonl
```

For a small initial capture, add `--max-posts 5`. This limits post lookups in the current segment;
listing requests and previously completed posts do not count. Omit it to walk the entire archive.

```sh
archivindex-substack-scraper archive --publication https://example.com \
  --max-posts 5 --output data/example-01.warc.gz
```

## User resolution

Resolve a numeric Substack user ID to its handle, profile slug, or complete public profile:

```sh
archivindex-substack-scraper resolve-handle 330239969
archivindex-substack-scraper resolve-slug 330239969
archivindex-substack-scraper resolve-user 330239969 > profile.json
```

`resolve-handle` sends `HEAD /profile/{id}` and reads the `/@handle` redirect without following it.
`resolve-user` then requests `/api/v1/user/{handle}/public_profile`. `resolve-slug` uses the same
requests and selects the profile's `slug` field, which can differ from its handle. The profile's
numeric ID must match the requested user. Profile JSON retains all fields, including unknown nested
data and explicit nulls; only the ID and slug have a fixed model schema.

Resolved handles and slugs go to stdout as one line; profiles as one JSON object. A missing user,
an existing user without a handle, a profile returning 404, or an empty slug produces no stdout and
exits successfully, with an explanation on stderr. The handle command distinguishes a missing user
(404) from an existing user without a handle (200). Unexpected statuses, malformed redirects,
invalid profiles, and incomplete captures exit unsuccessfully. Resolution is public and does not
read `SUBSTACK_COOKIE`.

Look up a handle directly to print its numeric user ID and profile slug as `id-slug`:

```sh
archivindex-substack-scraper lookup-handle ssmanhh
archivindex-substack-scraper lookup-handle @ssmanhh
```

Both forms request `/api/v1/user/ssmanhh/public_profile` directly, without a HEAD lookup. Output
has the form `330239969-ss-man`, followed by a newline. An empty profile slug produces `id-`.
A missing profile (404) produces no stdout and exits successfully, with an explanation on stderr.
Invalid handles, unexpected statuses, malformed profiles, and incomplete captures are errors.

All profile lookup and resolution commands, and `archive`, accept `--proxy` and `--show-ip`:

```sh
archivindex-substack-scraper resolve-user 330239969 \
  --proxy socks5h://127.0.0.1:1080 --show-ip > profile.json
```

`socks5h://` sends destination hostnames to the proxy for DNS resolution; `socks5://` resolves
them locally. Both accept optional `user:password@host:port` authentication and default to port
1080. The proxy applies to every request made by the command, including the IP check and any
retries. Proxy failures never fall back to direct connections. Without `--proxy`, commands connect
directly and ignore environment proxy settings. Proxy credentials are excluded from HTTP requests
and WARC metadata; recorded `warcinfo` identifies the configured proxy URI with its credentials
removed.

`--show-ip` queries `https://api.ipify.org/` through the same transport and prints
`Public IP address: ...` to stderr before contacting Substack. It also works without a proxy.
The check makes one attempt, accepts IPv4 or IPv6, and stops the command on connection errors,
unsuccessful statuses, redirects, invalid addresses, or truncated responses. The IP check is not
included in the retained WARC and never receives the publication cookie. Stdout remains reserved
for command results. For publication capture:

```sh
archivindex-substack-scraper archive --publication on --output data/on.warc.gz \
  --proxy socks5h://127.0.0.1:1080 --show-ip
```

Add `--output` to retain the lookup in a WARC file:

```sh
archivindex-substack-scraper resolve-user 330239969 --output data/profile.warc.gz > profile.json
archivindex-substack-scraper resolve-handle 330239969 --output data/handle.warc
```

All network requests use `archivindex-archiver`. Resolution WARCs contain any HEAD exchange, profile
GET, and retry responses, including failed lookups and 404s. Without `--output`, a temporary WARC is
created and removed after resolution. Output paths must be new, with existing parent directories.
The archiver writes through a `.partial` file and publishes the WARC when recording finishes;
recording failures may leave the partial file for recovery. A `.gz` suffix or `--gzip` enables
compression. The `export` and `inspect` commands currently recognize publication captures only, so
use a general WARC reader for resolution archives.

Requests start at least 1.5 seconds apart, configurable with `--delay SECONDS`. This includes
retries and later lookups made with the same resolver. Each capture has a 30-second timeout and a 16
MiB response limit, including headers, configurable through `resolve::Config` in the scraper
library. The archiver retries transient transport failures, 429, 500, 502, 503, and 504 responses,
up to six attempts per request. Backoff starts at five seconds, doubles, and is capped at five
minutes. A valid `Retry-After` overrides backoff, subject to that cap. These rules differ from
`substarch`, which retries only 429 and 503 and stops when `Retry-After` exceeds its cap.
Publication capture uses the same transport, with its own pacing and no automatic retries.

The lookup routes and response semantics come from `substarch`. They are undocumented Substack web
API behavior; the migration tests use local servers rather than live requests. Profile archiving
through these commands does not include the old feed traversal or user-extraction commands.

## Capture and resume

The downloader requests `/api/v1/archive?sort=new&offset=0&limit=20`, then fetches each distinct
post from `/api/v1/posts/{slug}`. Listings generally contain summaries with null body fields, so
both responses are archived. Pagination advances by the number of returned summaries until an empty
page is reached. Short pages do not end the run, since the service may cap the requested page size.
`--page-size` accepts 1 through 100. A page containing only previously seen post IDs stops the run
with an error to prevent an endless pagination loop.

Requests start at least two seconds apart by default. `--delay SECONDS` overrides this interval;
the default is a conservative local setting, not a documented service quota. A terminal spinner on
stderr shows elapsed time and completed post lookups. It is hidden when stderr is not a terminal.

Every capture is flushed to `<output>.partial` before the next request. The requested output file
appears when the run finishes, including when an HTTP or validation error stops it. Resume into a
**new** file, supplying all previous segments from the session:

```sh
archivindex-substack-scraper archive --publication on \
  --resume-from data/on-01.warc.gz --output data/on-02.warc.gz

archivindex-substack-scraper export data/on-01.warc.gz data/on-02.warc.gz > data/on.jsonl
```

Resume first retries unfinished post URLs discovered in earlier listing pages, then scans current
listings from offset zero. Valid post responses and complete post 404s are skipped, keyed by the
full request URL. Evidence from another publication origin does not suppress this publication's
requests. Listing pages are fetched again, including on a fully resumed run. This avoids treating
old offsets as stable after new posts appear. It also recovers posts listed in earlier segments
even if they have since disappeared from the current archive.

The API uses live offset pagination, so this is not a point-in-time snapshot. Concurrent deletions
or changes to listing order can still cause omissions within a run. Start a fresh capture to refresh
post versions or retry previously completed 404s. Supply the same publication origin and
authentication scope when resuming; changing between a custom domain and a Substack subdomain starts
a separate set of lookups.

HTTP 401, 403, 429, redirects, server errors, malformed JSON, invalid post data, and transport
errors stop the run with a nonzero exit code. There are no automatic retries. Complete HTTP
responses are retained before reporting errors, including `Retry-After` and `Location` when
supplied. A listing 404 is an error, since it does not establish that the publication is empty. Only
a post 404 counts as a completed missing lookup.

Output files are created exclusively and never overwritten or appended to. An existing output or
its `.partial` file prevents capture. The parent directory must exist. Gzip is enabled by `.gz` or
`--gzip`; readers detect compression from file bytes. A hard termination, disk failure, or failure
to publish the final output can leave the `.partial` file for recovery. A readable partial file can
be passed to `--resume-from` with a new output path. Broken WARC framing requires repair before
resume.

## Authentication and content scope

Public access is the default. To archive content available to a logged-in session, set
`SUBSTACK_COOKIE` to its Cookie header value, for example the `substack.sid` cookie. The downloader
sends it only to the configured publication origin and does not follow redirects or URLs embedded
in API responses.

**Cookie headers are preserved in WARC request records**, following the Gravatar project's approach
to authenticated HTTP capture. Treat authenticated archives as credentials-bearing files. Keep the
same access scope across resumed segments. To capture additional content after gaining access,
start a fresh capture without `--resume-from`.

The archive preserves whatever the API returns for the requesting session. A valid post response
can contain a preview, null body, or paid-audience metadata; completion does not imply access to the
full post. This tool does not bypass access restrictions. HTML and embedded metadata are preserved
inside JSON. Image, audio, and video binaries are not downloaded. The publication downloader does
not fetch comments, Notes, chats, standalone profiles, or publication discovery data.

These publication routes are an undocumented web API with no schema stability guarantee. The
implementation was checked against public responses from `on.substack.com` on September 15, 2026.
The separate `/api/v1/publication` route did not allow public access in that check and is not used.

## Export and archive format

Export writes one JSON object per valid post capture, with `kind`, `url`, `captured_at`, and `data`
fields. `data` preserves the original JSON, including unknown fields and explicit nulls. Captures
remain in input-file and record order, including repeated versions. Add `--include-pages` to export
the listing responses as well. `inspect` reports valid posts, listing pages, distinct completed post
URLs, and complete response counts by HTTP status, including failed responses.

```sh
jq -r '[.data.id, .data.title, .data.canonical_url] | @csv' data/on.jsonl
archivindex-substack-scraper export --include-pages data/on-01.warc.gz > data/all-responses.jsonl
```

Each file starts with the archiver's WARC 1.1 `warcinfo` record, identifying the session, software,
User-Agent, and any proxy without its credentials. Captured exchanges have linked `request`,
`response`, and `metadata` records. Responses link to requests through `WARC-Concurrent-To`, and
metadata links to responses. Metadata records timing and discovery provenance. Each post request
names the listing that discovered it when that listing belongs to the current segment.

The archiver records HTTP request and response bytes, including response transfer framing. The
reader decodes transfer framing when extracting JSON. Publication requests have a 60-second socket
idle timeout and total capture time limit, excluding DNS, and a 16 MiB response limit including
headers. Configure these through `Config::timeout` and `Config::max_response_bytes` in the library;
`max_response_bytes` replaces the former body-only `max_body_bytes` setting.

Responses cut short by a disconnect, timeout, or size limit are retained with `WARC-Truncated`.
They stop capture and never count as completed posts, even if the captured prefix is valid JSON or
the status is 404. A failure before response headers arrive reports an error without an HTTP
exchange to record. Earlier captures remain available for resume.

Offline readers recognize the supported API paths on any HTTP(S) origin, including custom domains,
and skip unrelated paths, revisits, and records marked truncated. The downloader always stores full
responses and does not produce revisits. Reading buffers one archive's extracted JSON in memory.

## Development

Install the workspace policy checker at the revision used by CI:

```console
cargo install --locked --git https://github.com/travisbrown/archivindex \
  --rev 527867ddf5b1eaaff23413a0a40da4a62d8e360e cargo-archivindex-build
```

Run the development checks:

```console
cargo archivindex-build check
cargo +nightly fmt --all -- --check
taplo fmt --check --diff
taplo lint
rumdl check .
cargo deny --workspace check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
```

Formatting uses nightly rustfmt, Taplo 0.10, and rumdl 0.2.73, matching the sibling projects.
`cargo archivindex-build sync` applies shared configuration fixes; run the formatters and checks
afterward. Tests use synthetic data and local HTTP servers; they require no live Substack access or
credentials. CI checks Rust 1.98 compatibility and tests both locked dependencies and the newest
compatible releases. `Cargo.lock` records resolved dependencies, including the shared Archivindex
libraries' Git revisions.

## API references

- [Live publication archive endpoint](https://on.substack.com/api/v1/archive?sort=new&offset=0&limit=1)
- [Live post detail endpoint used for verification](https://on.substack.com/api/v1/posts/shea-serrano-podcast)
- [Independent Python client's archive implementation](https://github.com/NHagar/substack_api/blob/master/substack_api/newsletter.py)
- [Independent Python client's post implementation](https://github.com/NHagar/substack_api/blob/master/substack_api/post.py)

## License

GNU General Public License, version 3 only; see [LICENSE](LICENSE).
