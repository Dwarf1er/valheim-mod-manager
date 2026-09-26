//! Multi-source mod resolution: Thunderstore and Hexium.
//!
//! Both sources serve the same experimental Thunderstore package-index JSON
//! *body* shape, behind the [`ModSource`](crate::sources::ModSource) trait.
//! The trait boundary, not the API shape, is what makes this a real
//! abstraction: callers (`install`, `search`, `update`) work against `&dyn
//! ModSource` and never construct a client themselves, so a future source
//! that is not Thunderstore-API-shaped can implement the trait however it
//! needs to.
//!
//! The two sources are *not* fetched identically, though: live testing found
//! that Hexium's index endpoint never sends a `Last-Modified` response
//! header, unlike Thunderstore's. `ThunderstoreClient::get_manifest()` and
//! `refresh_index()` require that header unconditionally (it drives their
//! on-disk freshness cache, comparing it against what was cached last time so
//! a large index need not be re-parsed on every run) and hard-error without
//! it. Hexium's own index is small enough that it doesn't need that caching
//! layer in the first place, so [`HexiumSource`] bypasses it entirely: it
//! fetches and parses the index itself on every call, straight into
//! `Vec<Package>`, the same public, plain-`Deserialize` type the engine
//! itself parses its index into. Everything downstream of that — resolution,
//! downloads, install — is still the engine's own machinery; only the
//! manifest *fetch* for Hexium is bespoke.
//!
//! `mods.yml`'s schema is the engine's own and has no room for "which source did
//! this come from", so that attribution is tracked in a sidecar file,
//! `.vmm_sources.json`, next to it (see
//! [`sources_file`](crate::sources::sources_file)).

use crate::error::{AppError, AppResult};
use crate::target::Target;
use async_trait::async_trait;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thunderstore_engine::client::ThunderstoreClient;
use thunderstore_engine::models::{Package, PackageIndex};
use thunderstore_engine::progress::ProgressReporter;

/// Thunderstore's canonical host.
pub const THUNDERSTORE_BASE_URL: &str = "https://thunderstore.io";
/// The Valheim community slug on Thunderstore.
pub const THUNDERSTORE_COMMUNITY: &str = "valheim";
/// Hexium's host.
pub const HEXIUM_BASE_URL: &str = "https://valheim.hexium.gg";
/// Hexium's experimental package-index URL, the same JSON shape Thunderstore's
/// `/c/valheim/api/v1/package/` serves.
pub const HEXIUM_PACKAGE_INDEX_URL: &str = "https://valheim.hexium.gg/api/v1/package/";

/// A mod repository vmm can install from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum SourceId {
  /// The default source, `thunderstore.io`'s Valheim community.
  Thunderstore,
  /// The opt-in second source, `valheim.hexium.gg`.
  Hexium,
}

impl std::fmt::Display for SourceId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let name = match self {
      Self::Thunderstore => "thunderstore",
      Self::Hexium => "hexium",
    };

    write!(f, "{name}")
  }
}

impl std::str::FromStr for SourceId {
  type Err = AppError;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s.to_ascii_lowercase().as_str() {
      "thunderstore" => Ok(Self::Thunderstore),
      "hexium" => Ok(Self::Hexium),
      other => Err(AppError::advice(
        format!("{other:?} is not a known mod source."),
        "Nothing was changed. Known sources are `thunderstore` and `hexium`.",
        &[],
      )),
    }
  }
}

/// The abstraction over a mod repository.
///
/// Every method call site works against `&dyn ModSource` / `Vec<Box<dyn
/// ModSource>>` rather than constructing a [`ThunderstoreClient`] directly, so
/// adding a third source never touches `install`, `search`, or `update`.
///
/// `async_trait` rather than a native `async fn` in the trait: callers need
/// `dyn ModSource`, and native `async fn` in traits is not dyn-safe.
#[async_trait]
pub trait ModSource: Send + Sync {
  /// Which source this is.
  fn id(&self) -> SourceId;

  /// This source's package manifest, loading from cache when it is fresh.
  async fn manifest(&self) -> AppResult<Vec<Package>>;

  /// Forces a network refresh of this source's manifest.
  async fn refresh(&self) -> AppResult<Vec<Package>>;

  /// The client to use for downloading files. `install_batch`'s downloads are
  /// not scoped to a client's own `base_url` (`download_file` does a plain
  /// `client.get(url)`), so any one source's client can download files
  /// resolved from any other source.
  fn download_client(&self) -> &ThunderstoreClient;
}

/// Materializes every package a client's index holds into a `Vec<Package>`.
async fn manifest_of(client: &ThunderstoreClient) -> AppResult<Vec<Package>> {
  let index = client.get_manifest().await?;

  Ok(
    (0..index.len())
      .filter_map(|idx| index.get_package_at(idx))
      .collect(),
  )
}

/// Materializes every package after forcing a network refresh.
async fn refresh_of(client: &ThunderstoreClient) -> AppResult<Vec<Package>> {
  let index = client.refresh_index().await?;

  Ok(
    (0..index.len())
      .filter_map(|idx| index.get_package_at(idx))
      .collect(),
  )
}

/// Thunderstore's Valheim community, the default source.
pub struct ThunderstoreSource(pub ThunderstoreClient);

#[async_trait]
impl ModSource for ThunderstoreSource {
  fn id(&self) -> SourceId {
    SourceId::Thunderstore
  }

  async fn manifest(&self) -> AppResult<Vec<Package>> {
    manifest_of(&self.0).await
  }

  async fn refresh(&self) -> AppResult<Vec<Package>> {
    refresh_of(&self.0).await
  }

  fn download_client(&self) -> &ThunderstoreClient {
    &self.0
  }
}

/// Ceiling on Hexium's package-index response body, so a misbehaving or
/// compromised host cannot exhaust memory. Generous: Hexium's whole index is
/// far smaller than Thunderstore's, which is also why it carries none of
/// `ThunderstoreClient`'s on-disk freshness cache in the first place.
const HEXIUM_MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

/// The network timeout applied to Hexium's manifest fetch, matching
/// `ThunderstoreClient`'s own default.
const HEXIUM_HTTP_TIMEOUT: Duration = Duration::from_secs(60);

/// The opt-in Hexium source.
///
/// Wraps a [`ThunderstoreClient`] purely for downloads (see
/// [`ModSource::download_client`]) — its own manifest-fetching methods are
/// never called, since they require a `Last-Modified` response header
/// Hexium's live index does not send (see the module docs). `manifest`/
/// `refresh` instead fetch and parse the index directly with a bare HTTP
/// client, using [`ThunderstoreClient::package_index_url`] so both stay
/// pointed at the same URL.
pub struct HexiumSource {
  client: ThunderstoreClient,
  http: reqwest::Client,
}

impl HexiumSource {
  /// Wraps `client`, built with [`ThunderstoreClient::package_index_url`]
  /// pointed at Hexium's index (see [`build_sources`]).
  pub fn new(client: ThunderstoreClient) -> AppResult<Self> {
    let http = reqwest::Client::builder()
      .connect_timeout(HEXIUM_HTTP_TIMEOUT)
      .timeout(HEXIUM_HTTP_TIMEOUT)
      .build()
      .map_err(|e| AppError::Other(format!("building Hexium's HTTP client: {e}")))?;

    Ok(Self { client, http })
  }

  /// Fetches and parses Hexium's package index fresh from the network, with
  /// no on-disk caching: its index is small enough not to need one, which is
  /// also why the endpoint carries none of the freshness-check machinery
  /// `ThunderstoreClient` relies on.
  async fn fetch(&self) -> AppResult<Vec<Package>> {
    let url = self.client.package_index_url();

    let response = self
      .http
      .get(url)
      .send()
      .await
      .map_err(|e| AppError::Other(format!("fetching Hexium's package index: {e}")))?
      .error_for_status()
      .map_err(|e| AppError::Other(format!("Hexium's package index request failed: {e}")))?;

    let bytes = response
      .bytes()
      .await
      .map_err(|e| AppError::Other(format!("reading Hexium's package index: {e}")))?;

    if bytes.len() > HEXIUM_MAX_MANIFEST_BYTES {
      return Err(AppError::Other(format!(
        "Hexium's package index is {} bytes, over the {HEXIUM_MAX_MANIFEST_BYTES}-byte cap",
        bytes.len()
      )));
    }

    serde_json::from_slice(&bytes)
      .map_err(|e| AppError::Other(format!("parsing Hexium's package index: {e}")))
  }
}

#[async_trait]
impl ModSource for HexiumSource {
  fn id(&self) -> SourceId {
    SourceId::Hexium
  }

  async fn manifest(&self) -> AppResult<Vec<Package>> {
    self.fetch().await
  }

  async fn refresh(&self) -> AppResult<Vec<Package>> {
    // There is no cache to bypass: every fetch is already live.
    self.fetch().await
  }

  fn download_client(&self) -> &ThunderstoreClient {
    &self.client
  }
}

/// Builds the client + wrapper for one configured source, sharing `cache_dir`
/// and `progress` with every other source. The engine namespaces each index by
/// a CRC32 of its own URL, so sharing one `cache_dir` across sources cannot
/// collide.
fn build_source(
  id: SourceId,
  cache_dir: &Path,
  progress: Arc<dyn ProgressReporter>,
) -> AppResult<Box<dyn ModSource>> {
  let source: Box<dyn ModSource> = match id {
    SourceId::Thunderstore => {
      let client = ThunderstoreClient::builder()
        .base_url(THUNDERSTORE_BASE_URL)
        .community(THUNDERSTORE_COMMUNITY)
        .cache_dir(cache_dir)
        .progress(progress)
        .build()?;

      Box::new(ThunderstoreSource(client))
    }
    SourceId::Hexium => {
      let client = ThunderstoreClient::builder()
        .base_url(HEXIUM_BASE_URL)
        .package_index_url(HEXIUM_PACKAGE_INDEX_URL)
        .cache_dir(cache_dir)
        .progress(progress)
        .build()?;

      Box::new(HexiumSource::new(client)?)
    }
  };

  Ok(source)
}

/// Builds every configured source, sharing one cache directory and progress
/// reporter. The one factory call sites use.
pub fn build_sources(
  ids: &[SourceId],
  cache_dir: &Path,
  progress: Arc<dyn ProgressReporter>,
) -> AppResult<Vec<Box<dyn ModSource>>> {
  ids
    .iter()
    .map(|id| build_source(*id, cache_dir, progress.clone()))
    .collect()
}

/// Borrows every source in `sources` as `&dyn ModSource`, so a caller can hand
/// [`merged_manifest`] a filtered subset (e.g. `search --source hexium`)
/// without needing `ModSource` to be `Clone`.
pub fn as_refs(sources: &[Box<dyn ModSource>]) -> Vec<&dyn ModSource> {
  sources.iter().map(|source| source.as_ref()).collect()
}

/// The client every install/update/import pipeline downloads through: whichever
/// source is configured first. `install_batch`'s downloads are not scoped to a
/// client's own `base_url`, so any one source's client can download a file
/// resolved from any other source (see [`ModSource::download_client`]).
pub fn first_download_client(sources: &[Box<dyn ModSource>]) -> AppResult<&ThunderstoreClient> {
  sources
    .first()
    .map(|source| source.download_client())
    .ok_or_else(|| {
      AppError::advice(
        "no mod source is configured.",
        "Nothing was changed. Set `[sources] enabled` in your config to at \
         least one of `thunderstore` or `hexium`.",
        &[],
      )
    })
}

/// Fetches and merges every source's manifest into one [`PackageIndex`], plus a
/// `full_name -> SourceId` map recording which source each package came from.
///
/// When the same `full_name` exists on more than one source, the one with the
/// later `date_updated` wins; the losing source's version is dropped from the
/// merged list entirely, so the result never carries two versions of the same
/// full name (`PackageIndex`'s unique-full-name expectation holds if this is
/// ever round-tripped through the on-disk cache). The collision is logged at
/// `info`.
///
/// `forced` overrides that rule for specific full names (`vmm install --source
/// hexium Owner-Mod`): only the named source's version of that name is
/// considered at all, regardless of `date_updated`. Pass an empty map for the
/// ordinary (unforced) merge.
pub async fn merged_manifest(
  sources: &[&dyn ModSource],
  refresh: bool,
  forced: &HashMap<String, SourceId>,
) -> AppResult<(PackageIndex, HashMap<String, SourceId>)> {
  let mut by_full_name: HashMap<String, (Package, SourceId)> = HashMap::new();

  for source in sources {
    let packages = match refresh {
      true => source.refresh().await?,
      false => source.manifest().await?,
    };

    for package in packages {
      let Some(full_name) = package.full_name.clone() else {
        continue;
      };

      if let Some(&wanted) = forced.get(&full_name) {
        if source.id() == wanted {
          by_full_name.insert(full_name, (package, source.id()));
        }

        continue;
      }

      match by_full_name.get(&full_name) {
        Some((existing, existing_source)) if existing.date_updated >= package.date_updated => {
          tracing::info!(
            "{full_name}: keeping the version from {existing_source}, already at least as \
             recent as the one {} offers",
            source.id()
          );
        }
        Some((_, existing_source)) => {
          tracing::info!(
            "{full_name}: {} has a newer version than {existing_source}; using it instead",
            source.id()
          );
          by_full_name.insert(full_name, (package, source.id()));
        }
        None => {
          by_full_name.insert(full_name, (package, source.id()));
        }
      }
    }
  }

  let mut source_map = HashMap::with_capacity(by_full_name.len());
  let mut packages = Vec::with_capacity(by_full_name.len());

  for (full_name, (package, source_id)) in by_full_name {
    source_map.insert(full_name, source_id);
    packages.push(package);
  }

  Ok((packages.into(), source_map))
}

/// The sidecar file recording which source each installed mod came from.
///
/// `mods.yml`'s schema is the engine's own (kept r2modman/Gale-compatible), so
/// there is no room in it for a source other than the implicit "Thunderstore"
/// every reader already assumes. This lives beside it instead, and is treated
/// the same way vmm treats every other record: missing reads as empty.
pub fn sources_file(target: &Target) -> PathBuf {
  target.dir.join(".vmm_sources.json")
}

/// Reads the sidecar, defaulting to empty when it is missing or unreadable —
/// the same "unreadable record reads as empty" convention `config.rs` and
/// `portability.rs` use, since which source an old install came from is
/// informational, not load-bearing the way `mods.yml` is.
pub fn read_sources(target: &Target) -> HashMap<String, SourceId> {
  let path = sources_file(target);

  let Ok(contents) = std::fs::read_to_string(&path) else {
    return HashMap::new();
  };

  serde_json::from_str(&contents).unwrap_or_default()
}

/// Writes the sidecar.
fn write_sources(target: &Target, sources: &HashMap<String, SourceId>) -> AppResult<()> {
  let path = sources_file(target);
  let serialized = serde_json::to_string_pretty(sources)?;

  std::fs::write(path, serialized)?;

  Ok(())
}

/// Records where one mod came from.
#[cfg_attr(not(test), allow(dead_code))]
pub fn record_source(target: &Target, name: &str, source: SourceId) -> AppResult<()> {
  record_sources(target, [(name.to_string(), source)])
}

/// Records where every one of `entries` came from, merging into whatever the
/// sidecar already holds.
pub fn record_sources(
  target: &Target,
  entries: impl IntoIterator<Item = (String, SourceId)>,
) -> AppResult<()> {
  let mut sources = read_sources(target);

  for (name, source) in entries {
    sources.insert(name, source);
  }

  write_sources(target, &sources)
}

/// Drops every sidecar entry not in `keep`, so it never outlives what
/// `mods.yml` records. Called after every successful uninstall.
pub fn prune_sources(target: &Target, keep: &HashSet<String>) -> AppResult<()> {
  let mut sources = read_sources(target);

  sources.retain(|name, _| keep.contains(name));

  write_sources(target, &sources)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::test_support::Fixture;

  #[test]
  fn source_id_round_trips_through_its_string_form() {
    assert_eq!(SourceId::Thunderstore.to_string(), "thunderstore");
    assert_eq!(SourceId::Hexium.to_string(), "hexium");

    assert_eq!(
      "thunderstore".parse::<SourceId>().unwrap(),
      SourceId::Thunderstore
    );
    assert_eq!("HEXIUM".parse::<SourceId>().unwrap(), SourceId::Hexium);
    assert!("nonsense".parse::<SourceId>().is_err());
  }

  #[test]
  fn the_sidecar_round_trips_through_record_read_and_prune() {
    let fixture = Fixture::new();
    let target = fixture.target();

    assert!(read_sources(&target).is_empty());

    record_sources(
      &target,
      [
        ("Owner-ModA".to_string(), SourceId::Thunderstore),
        ("Owner-ModB".to_string(), SourceId::Hexium),
      ],
    )
    .unwrap();

    let sources = read_sources(&target);

    assert_eq!(sources.get("Owner-ModA"), Some(&SourceId::Thunderstore));
    assert_eq!(sources.get("Owner-ModB"), Some(&SourceId::Hexium));

    // A single-name update merges into what is already there rather than
    // replacing it wholesale.
    record_source(&target, "Owner-ModC", SourceId::Hexium).unwrap();

    assert_eq!(read_sources(&target).len(), 3);

    let mut keep = HashSet::new();
    keep.insert("Owner-ModB".to_string());

    prune_sources(&target, &keep).unwrap();

    let pruned = read_sources(&target);

    assert_eq!(pruned.len(), 1);
    assert_eq!(pruned.get("Owner-ModB"), Some(&SourceId::Hexium));
  }

  #[test]
  fn a_missing_sidecar_reads_as_empty_not_an_error() {
    let fixture = Fixture::new();
    let target = fixture.target();

    assert!(!sources_file(&target).exists());
    assert!(read_sources(&target).is_empty());
  }

  #[test]
  fn merged_manifest_resolves_a_dependency_that_only_exists_on_one_source() {
    let fixture = Fixture::new();
    let sources = fixture.multi_sources();

    let (index, map) = tokio::runtime::Runtime::new()
      .unwrap()
      .block_on(merged_manifest(&as_refs(&sources), false, &HashMap::new()))
      .unwrap();

    // Owner-ModA (Thunderstore) and Hexium-OnlyMod (Hexium only) both resolve
    // out of the same merged index.
    assert!(index.find_index_by_full_name("Owner-ModA").is_some());
    assert!(index.find_index_by_full_name("Hexium-OnlyMod").is_some());
    assert_eq!(map.get("Owner-ModA"), Some(&SourceId::Thunderstore));
    assert_eq!(map.get("Hexium-OnlyMod"), Some(&SourceId::Hexium));
  }

  #[test]
  fn a_name_on_both_sources_resolves_to_the_more_recently_updated_one() {
    let fixture = Fixture::new();
    let sources = fixture.multi_sources();

    let (index, map) = tokio::runtime::Runtime::new()
      .unwrap()
      .block_on(merged_manifest(&as_refs(&sources), false, &HashMap::new()))
      .unwrap();

    // Owner-Shared is seeded with a newer `date_updated` on Hexium than on
    // Thunderstore (see `Fixture::multi_sources`), so Hexium's version wins and
    // Thunderstore's is dropped entirely rather than both surviving.
    assert_eq!(map.get("Owner-Shared"), Some(&SourceId::Hexium));

    let idx = index.find_index_by_full_name("Owner-Shared").unwrap();
    let package = index.get_package_at(idx).unwrap();

    assert_eq!(package.versions.len(), 1);
  }

  #[test]
  fn forcing_a_source_bypasses_the_collision_rule_for_that_name_only() {
    let fixture = Fixture::new();
    let sources = fixture.multi_sources();

    let mut forced = HashMap::new();
    forced.insert("Owner-Shared".to_string(), SourceId::Thunderstore);

    let (_, map) = tokio::runtime::Runtime::new()
      .unwrap()
      .block_on(merged_manifest(&as_refs(&sources), false, &forced))
      .unwrap();

    // Forced to Thunderstore even though Hexium's copy is newer.
    assert_eq!(map.get("Owner-Shared"), Some(&SourceId::Thunderstore));
    // Everything else still resolves by the ordinary rule.
    assert_eq!(map.get("Owner-ModA"), Some(&SourceId::Thunderstore));
    assert_eq!(map.get("Hexium-OnlyMod"), Some(&SourceId::Hexium));
  }
}
