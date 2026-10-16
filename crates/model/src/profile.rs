//! Public profile responses, retaining fields beyond those needed for identity resolution.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A response from `/api/v1/user/{handle}/public_profile`.
///
/// Only the identity and profile slug have a fixed schema. All remaining fields, including nested
/// publications, subscriptions, unknown fields, and explicit nulls, are preserved as JSON.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct PublicProfile {
    /// Numeric user identifier.
    pub id: u64,
    /// Profile URL slug, distinct from the handle. An empty slug is valid.
    pub slug: String,
    /// All other profile fields, using their original API names.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_preserve_nulls_unknown_fields_and_nested_data() {
        let value = serde_json::json!({
            "id": 42, "slug": "display-name", "handle": "differenthandle", "name": null,
            "primaryPublication": {"id": 7, "future": {"value": null}},
            "subscriptions": [], "future": [null, 1, "two"]
        });
        let profile: PublicProfile = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(profile).unwrap(), value);
        for invalid in [
            serde_json::json!({"error": "not a profile"}),
            serde_json::json!({"id": 42}),
            serde_json::json!({"id": 42, "slug": null}),
        ] {
            assert!(serde_json::from_value::<PublicProfile>(invalid).is_err());
        }
    }
}
