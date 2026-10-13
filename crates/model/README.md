# archivindex-substack-model

Data models for Substack publication API responses. `Post` covers both listing summaries and post
details, while `Byline` represents an author. Both retain unknown fields for API additions.

`Slug` validates post slugs before they are used as API path segments. Optional post fields accept
missing or null values; serializing a model omits those values. Use the original JSON when that
distinction matters.

`profile::PublicProfile` models the numeric user ID and profile slug, preserving every other field
as JSON. Unlike post slugs, profile slugs can be empty and do not share the post-slug alphabet.

See the [workspace README](../../README.md) for capture behavior and supported API routes.

## License

GNU General Public License, version 3 only; see [LICENSE](../../LICENSE).
