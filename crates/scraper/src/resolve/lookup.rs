use archivindex_archiver::session::{Capture, Driver, Inspection, Request};
use archivindex_substack_model::profile::PublicProfile;
use url::Url;

use super::{Handle, Lookup, profile_request, valid_handle};
use crate::Error;

impl Lookup {
    fn inspect_response(&mut self, capture: &Capture<'_>) -> Result<(), Error> {
        if capture.status == 404 {
            return Ok(());
        }
        if self.handle.is_some() {
            if capture.status != 200 {
                return Err(format!("unexpected profile HTTP status: {}", capture.status).into());
            }
            let profile: PublicProfile = serde_json::from_slice(capture.payload)?;
            if let Some(user_id) = self.user_id
                && profile.id != user_id
            {
                return Err(format!(
                    "profile ID {} does not match requested user {}",
                    profile.id, user_id
                )
                .into());
            }
            self.profile = Some(profile);
        } else if capture.status == 200 {
            self.handle = Some(Handle::Unset);
        } else if matches!(capture.status, 301 | 302 | 303 | 307 | 308) {
            let location = capture
                .header("location")
                .ok_or("redirect has no valid Location header")?;
            let target = Url::parse(capture.url)?.join(location)?;
            let path = target
                .path()
                .strip_suffix('/')
                .unwrap_or_else(|| target.path());
            let handle = path
                .strip_prefix("/@")
                .filter(|handle| valid_handle(handle));
            if target.origin() != self.base.origin()
                || !target.username().is_empty()
                || target.password().is_some()
                || target.query().is_some()
                || target.fragment().is_some()
                || handle.is_none()
            {
                return Err(format!("unexpected redirect location: {location}").into());
            }
            let handle = handle.expect("handle checked above");
            if self.full {
                self.pending = Some(profile_request(&self.base, handle, Some(capture.url))?);
            }
            self.handle = Some(Handle::Set(handle.into()));
        } else {
            return Err(format!("unexpected profile HTTP status: {}", capture.status).into());
        }
        Ok(())
    }
}

impl Driver for Lookup {
    fn next(&mut self) -> Option<Request> {
        self.pending.take()
    }

    fn inspect(&mut self, capture: &Capture<'_>) -> Inspection {
        match self.inspect_response(capture) {
            Ok(()) => Inspection::default(),
            Err(error) => Inspection::error(error.to_string()),
        }
    }
}
