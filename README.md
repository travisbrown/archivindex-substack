# archivindex-substack

Rust libraries and a command-line tool for downloading Substack publication posts, archiving HTTP
responses in WARC files, and exporting captured JSON for analysis. Follows the model / scraper / CLI
workspace layout of `archivindex-gravatar` and `archivindex-wordpress`, using their shared
`archivindex-warc` library.

| Crate                                                    | Purpose                                                           |
| -------------------------------------------------------- | ----------------------------------------------------------------- |
| [`archivindex-substack-model`](crates/model/)            | Post and author models; validated post slugs                      |
| [`archivindex-substack-scraper`](crates/scraper/)        | Pagination, HTTP capture, WARC output, archive reading and resume |
| [`archivindex-substack-scraper-cli`](tools/scraper-cli/) | `archive`, `export`, and `inspect` commands                       |

## Getting started

Requires Rust 1.97 or later. Install the CLI:

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

Every capture is flushed before the next request. Resume into a **new** file, supplying all previous
segments from the session:

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

Output files are created exclusively and never overwritten or appended to. The parent directory
must exist. Gzip is enabled by `.gz` or `--gzip`; readers detect compression from file bytes.
Malformed or truncated WARC framing causes a read error and requires repair before resume. A hard
termination or disk failure during a write can leave a partial final record.

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
inside JSON. Image, audio, and video binaries are not downloaded. Comments, Notes, chats, standalone
profiles, and publication discovery are outside the current capture scope.

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

Each file starts with a WARC 1.1 `warcinfo` record identifying the filename, software/version,
format, and User-Agent. Each request writes linked `request`, `response`, and `metadata` records.
All refer to the initial record through `WARC-Warcinfo-ID`; responses link to requests and metadata
links to both through `WARC-Concurrent-To`. Metadata contains `fetchTimeMs`, the time spent fetching
and preparing the response in milliseconds.

HTTP records contain reconstructed HTTP/1.1 headers and entity bytes, with transfer framing
normalized after HTTP decoding. They are not byte-for-byte wire captures. Query strings are included
in archived request targets. Requests use a 60-second timeout and 16 MiB body limit by default,
configurable through the library. If the response body fails or exceeds the limit, only the request
and fetch-time metadata are retained, and the lookup remains eligible for resume.

Offline readers recognize the supported API paths on any HTTP(S) origin, including custom domains,
and skip unrelated paths, revisits, and records marked truncated. The downloader always stores full
responses and does not produce revisits. Reading buffers one archive's extracted JSON in memory.

## Development

Install the workspace policy checker at the revision used by CI:

```console
cargo install --locked --git https://github.com/travisbrown/archivindex \
  --rev f6e131cb8255090c5384f6cdcb099348247ad1ff cargo-archivindex-build
```

Run the development checks:

```console
cargo archivindex-build check
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features
RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
```

Formatting uses the same nightly rustfmt options as the sibling projects. Tests use synthetic data
and local HTTP servers; they require no live Substack access or credentials. CI also checks Rust
1.97 compatibility and the shared Archivindex workspace policy. `Cargo.lock` records resolved
dependencies, including the shared WARC library's Git revision.

## API references

- [Live publication archive endpoint](https://on.substack.com/api/v1/archive?sort=new&offset=0&limit=1)
- [Live post detail endpoint used for verification](https://on.substack.com/api/v1/posts/shea-serrano-podcast)
- [Independent Python client's archive implementation](https://github.com/NHagar/substack_api/blob/master/substack_api/newsletter.py)
- [Independent Python client's post implementation](https://github.com/NHagar/substack_api/blob/master/substack_api/post.py)

## License

GNU General Public License, version 3 only; see [LICENSE](LICENSE).
