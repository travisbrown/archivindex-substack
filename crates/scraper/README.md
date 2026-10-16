# archivindex-substack-scraper

Download Substack publication posts and preserve HTTP exchanges in plain or gzip WARC files.
`Downloader` fetches listing pages and post details sequentially, with configurable request spacing,
timeouts, response limits, SOCKS proxies, and authentication. All network requests use
`archivindex-archiver` sessions. Publication capture makes one attempt per request.

`read_archive` extracts valid JSON captures and resume evidence from existing archives. Pass that
evidence to `Downloader::archive` to skip completed posts while writing a new segment. Each exchange
is flushed before the next request. The archiver writes to `<output>.partial` and publishes the
output when the run ends. Incomplete responses remain eligible for resume.

Cookie headers are stored in WARC request records. Authenticated archives therefore contain
credentials and must be handled accordingly.

`resolve::Resolver` resolves numeric user IDs to handles, profile slugs, and complete public
profiles. It uses `archivindex-archiver` sessions for both the HEAD redirect and the profile GET,
without following redirects. Pass an output path to retain those exchanges and their retries in a
WARC; pass `None` to discard the temporary archive after resolution. Resolution settings, including
the archiver retry policy, belong to `resolve::Config`.

`Resolver::lookup_handle` fetches a public profile directly by handle, with or without one leading
`@`, and uses the same validation and WARC recording as numeric-ID resolution.

Set `Config::proxy` or `resolve::Config::proxy` to a `socks5://` or `socks5h://` URL to proxy all
requests. `Downloader::public_ip` and `Resolver::public_ip` check an IP service through the same
transport. IP checks never send the publication cookie. Proxy failures never fall back to direct
connections, and `socks5h://` resolves hostnames remotely.

See the [workspace README](../../README.md) for CLI usage, resume semantics, and capture
limitations.

## License

GNU General Public License, version 3 only; see [LICENSE](../../LICENSE).
