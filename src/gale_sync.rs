//! The gale-sync profile API: a *desired modlist* source.
//!
//! Unlike [`crate::sources::ModSource`], which answers "where do I download
//! this mod from", gale-sync answers "what should the modlist be right now". It
//! is also meant to be polled repeatedly (once per update cycle) rather than
//! imported once, so [`fetch_current`] is a plain fetch with no side effects on
//! the target; installing and pruning against the result is the
//! reconciliation phase's job.
//!
//! API reference: <https://github.com/Kesomannen/gale-sync/blob/master/docs/api.md>.
//! `GET /profile/{id}` is unauthenticated (only writes need a token) and
//! redirects to the profile's CDN copy: a ZIP holding an `export.r2x`
//! manifest, the same shape as an r2modman `.r2z`, so it is read with the
//! engine's own [`portability::read_export`] and can be handed straight to the
//! existing zip import path.

use crate::config::GaleSyncConfig;
use crate::error::{AppError, AppResult};
use std::time::Duration;
use thunderstore_engine::profile::portability;

/// Timeout for both connecting and the whole request, so an unreachable
/// gale-sync fails fast instead of stalling an unattended update cycle.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// The modlist a synced profile currently names.
#[derive(Debug, Clone)]
pub struct DesiredModlist {
  /// The synced profile's display name (it may change between updates).
  pub profile_name: String,
  /// Every mod the profile names, as `Owner-Name`.
  pub mods: Vec<String>,
  /// The profile's raw ZIP export, ready for
  /// [`portability::import_r2z_in`]-style installation.
  pub zip_bytes: Vec<u8>,
}

/// Fetches the configured sync profile's current desired modlist.
///
/// Fails with an advice-style error when no `profile_id` is configured or
/// gale-sync cannot be reached, so callers can decide whether to fall back to
/// "keep whatever is installed" (see [`GaleSyncConfig`]).
pub async fn fetch_current(config: &GaleSyncConfig) -> AppResult<DesiredModlist> {
  let id = config.profile_id.as_deref().ok_or_else(|| {
    AppError::advice(
      "no gale-sync profile is configured.",
      "Set `profile_id` under `[gale_sync]` in vmm_config.toml.",
      &[],
    )
  })?;

  fetch_profile(&config.base_url, id).await
}

/// Fetches profile `id` from the gale-sync API rooted at `base_url`.
async fn fetch_profile(base_url: &str, id: &str) -> AppResult<DesiredModlist> {
  let http = reqwest::Client::builder()
    .connect_timeout(HTTP_TIMEOUT)
    .timeout(HTTP_TIMEOUT)
    .build()
    .map_err(|e| AppError::Other(format!("building gale-sync's HTTP client: {e}")))?;

  let url = format!("{}/profile/{}", base_url.trim_end_matches('/'), id);

  let response = http
    .get(&url)
    .send()
    .await
    .map_err(|e| AppError::Other(format!("fetching gale-sync profile {id}: {e}")))?
    .error_for_status()
    .map_err(|e| AppError::Other(format!("gale-sync profile {id} request failed: {e}")))?;

  let zip_bytes = response
    .bytes()
    .await
    .map_err(|e| AppError::Other(format!("reading gale-sync profile {id}: {e}")))?
    .to_vec();

  let export = portability::read_export(&zip_bytes)?;

  Ok(DesiredModlist {
    profile_name: export.profile_name,
    mods: export.mods.into_iter().map(|entry| entry.name).collect(),
    zip_bytes,
  })
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::io::Write;

  fn export_zip(manifest: &str) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
      let mut zip = zip::ZipWriter::new(&mut buf);
      zip
        .start_file("export.r2x", zip::write::SimpleFileOptions::default())
        .unwrap();
      zip.write_all(manifest.as_bytes()).unwrap();
      zip.finish().unwrap();
    }
    buf.into_inner()
  }

  const MANIFEST: &str = "profileName: Live\nmods:\n  - name: Owner-One\n    version:\n      major: 1\n      minor: 2\n      patch: 3\n    enabled: true\n  - name: Owner-Two\n    version:\n      major: 0\n      minor: 1\n      patch: 0\n    enabled: true\n";

  #[tokio::test]
  async fn fetches_and_parses_the_current_modlist_following_a_redirect() {
    let mut server = mockito::Server::new_async().await;
    let cdn = server
      .mock("GET", "/cdn/abc")
      .with_body(export_zip(MANIFEST))
      .create_async()
      .await;
    let api = server
      .mock("GET", "/profile/abc")
      .with_status(302)
      .with_header("location", &format!("{}/cdn/abc", server.url()))
      .create_async()
      .await;

    let desired = fetch_profile(&server.url(), "abc").await.unwrap();

    assert_eq!(desired.profile_name, "Live");
    assert_eq!(desired.mods, vec!["Owner-One", "Owner-Two"]);
    cdn.assert_async().await;
    api.assert_async().await;
  }

  #[tokio::test]
  async fn an_http_error_is_reported() {
    let mut server = mockito::Server::new_async().await;
    let _m = server
      .mock("GET", "/profile/missing")
      .with_status(404)
      .create_async()
      .await;

    let err = fetch_profile(&server.url(), "missing").await.unwrap_err();

    assert!(err.to_string().contains("missing"));
  }

  #[tokio::test]
  async fn an_unconfigured_profile_id_is_refused() {
    let err = fetch_current(&GaleSyncConfig::default()).await.unwrap_err();

    assert!(err.to_string().contains("no gale-sync profile"));
  }
}
