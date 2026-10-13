# archivindex-substack-scraper

Download Substack publication posts and preserve HTTP exchanges in plain or gzip WARC files.
`Downloader` fetches listing pages and post details sequentially, with configurable request spacing,
timeouts, body limits, and authentication.

`read_archive` extracts valid JSON captures and resume evidence from existing archives. Pass that
evidence to `Downloader::archive` to skip completed posts while writing a new segment. Each exchange
is flushed before validation or the next request.

Cookie headers are stored in WARC request records. Authenticated archives therefore contain
credentials and must be handled accordingly.

See the [workspace README](../../README.md) for CLI usage, resume semantics, and capture
limitations.

## License

GNU General Public License, version 3 only; see [LICENSE](../../LICENSE).
