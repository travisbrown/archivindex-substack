//! Substack post and public profile data, retaining unmodeled API fields.

pub mod profile;

use std::collections::BTreeMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A post slug that can safely be used as a single API path segment.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Slug(String);

/// A slug is empty or contains characters outside the supported URL segment alphabet.
#[derive(Debug, thiserror::Error)]
#[error("expected a post slug containing ASCII letters, digits, hyphens, or underscores")]
pub struct InvalidSlug;

impl FromStr for Slug {
    type Err = InvalidSlug;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        if input.is_empty()
            || !input
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            Err(InvalidSlug)
        } else {
            Ok(Self(input.to_owned()))
        }
    }
}

impl<'de> serde::de::Deserialize<'de> for Slug {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for Slug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A post summary or detail response. Body fields may be absent or null, including for paid posts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Post {
    /// Post identifier.
    pub id: u64,
    /// Publication identifier.
    pub publication_id: u64,
    /// URL slug.
    pub slug: Slug,
    /// Display title.
    pub title: String,
    /// Subtitle, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subtitle: Option<String>,
    /// Publication timestamp as supplied by the API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_date: Option<String>,
    /// Canonical public URL. Downloads stay on the configured publication origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_url: Option<String>,
    /// Content category, such as newsletter or podcast.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub post_type: Option<String>,
    /// Audience label, such as everyone or `only_paid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// HTML made available to the requesting session, possibly only a preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_html: Option<String>,
    /// Structured body, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_json: Option<Value>,
    /// Cover image URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_image: Option<String>,
    /// Podcast media URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub podcast_url: Option<String>,
    /// Published author bylines.
    #[serde(
        default,
        rename = "publishedBylines",
        skip_serializing_if = "Option::is_none"
    )]
    pub published_bylines: Option<Vec<Byline>>,
    /// Tags, preserved without imposing a schema on tag metadata.
    #[serde(default, rename = "postTags", skip_serializing_if = "Option::is_none")]
    pub post_tags: Option<Vec<Value>>,
    /// Additional fields supplied by the API.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

/// An author byline included in a post response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Byline {
    /// User identifier.
    pub id: u64,
    /// Author name.
    pub name: String,
    /// Public profile handle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
    /// Profile photograph URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo_url: Option<String>,
    /// Biography.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bio: Option<String>,
    /// Additional author and publication metadata.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_cannot_escape_the_endpoint_even_when_deserialized() {
        for input in [
            "",
            "..",
            "a/b",
            "x?y",
            "x#y",
            "a%2fb",
            "a b",
            " a",
            "https://x",
        ] {
            assert!(input.parse::<Slug>().is_err());
            assert!(serde_json::from_value::<Slug>(Value::String(input.into())).is_err());
        }
        assert_eq!("a-post_2".parse::<Slug>().unwrap().to_string(), "a-post_2");
    }

    #[test]
    fn unknown_post_and_author_fields_round_trip() {
        let value = serde_json::json!({
            "id": 42, "publication_id": 1, "slug": "example", "title": "Example",
            "future": {"a": null}, "publishedBylines": [{"id": 7, "name": "Author", "future": 3}]
        });
        let post: Post = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(post).unwrap(), value);
        assert!(serde_json::from_str::<Post>(r#"{"error":"unexpected"}"#).is_err());
    }
}
