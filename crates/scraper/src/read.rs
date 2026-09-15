use std::fs::File;
use std::io::{BufRead, Read};
use std::path::Path;

use archivindex_substack_model::Post;
use archivindex_warc::io::read::WarcReader;
use archivindex_warc::record::extension::NoExtension;
use archivindex_warc::record::{Record, payload};
use serde_json::Value;
use url::Url;

use super::{ArchiveContents, ArchivedCapture, Endpoint, Error};

#[expect(
    clippy::redundant_pub_crate,
    reason = "Limit access to the parent module."
)]
pub(super) fn read(path: &Path) -> Result<ArchiveContents, Error> {
    let mut magic = [0; 2];
    let size = File::open(path)?.read(&mut magic)?;
    if size == 2 && magic == [0x1f, 0x8b] {
        collect(WarcReader::from_path_gzip(path)?)
    } else {
        collect(WarcReader::from_path(path)?)
    }
}

fn collect<R: BufRead + Read>(reader: WarcReader<R>) -> Result<ArchiveContents, Error> {
    let mut result = ArchiveContents::default();
    for record in reader.iter_records::<NoExtension>().records() {
        let Record::Response { header, body } = record? else {
            continue;
        };
        if header.core.truncated.is_some() {
            continue;
        }
        let url = header.target_uri.into_string();
        let Ok(parsed) = Url::parse(&url) else {
            continue;
        };
        let Some(endpoint) = Endpoint::from_url(&parsed) else {
            continue;
        };
        let response = archivindex_warc::record::http::ResponseMetadata::parse(&body)
            .ok_or("invalid archived HTTP response")?;
        let entity = payload::entity_body(&body)?;
        if response.header("transfer-encoding").is_none()
            && let Some(length) = response.header("content-length")
            && std::str::from_utf8(length)?.trim().parse::<usize>()? != entity.len()
        {
            continue;
        }
        *result.status_counts.entry(response.status).or_default() += 1;
        if response.status == 404 && matches!(endpoint, Endpoint::Post(_)) {
            result.resume.completed.insert(parsed.to_string());
        } else if response.status == 200 {
            let Ok(data) = serde_json::from_slice::<Value>(&entity) else {
                continue;
            };
            let Ok(kind) = endpoint.validate(&data) else {
                continue;
            };
            match endpoint {
                Endpoint::Post(_) => {
                    result.resume.completed.insert(parsed.to_string());
                }
                Endpoint::Archive => {
                    for post in serde_json::from_value::<Vec<Post>>(data.clone())? {
                        let post_url = parsed.join(&format!("/api/v1/posts/{}", post.slug))?;
                        result.resume.discovered.insert(post_url.to_string());
                    }
                }
            }
            result.captures.push(ArchivedCapture {
                kind,
                url,
                captured_at: header.core.date.to_string(),
                data,
            });
        }
    }
    Ok(result)
}
