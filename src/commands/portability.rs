use crate::error::{AppError, AppResult};
use crate::sources::{ModSource, SourceId};
use crate::target::{GAME, Target};
use base64::Engine as _;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use thunderstore_engine::client::ThunderstoreClient;
use thunderstore_engine::ecosystem::Ecosystem;
use thunderstore_engine::profile::{
  self, modlist,
  portability::{self, ImportSource},
};

/// Writes the target's mods and config to an `.r2z` under its exports directory.
pub fn export_file(target: &Target) -> AppResult<()> {
  let path = portability::export_to_file_in(
    &target.dir,
    target.name(),
    &target.exports_dir(),
    thunderstore_engine::profile::modlist::now_millis(),
  )?;

  println!("wrote {}", path.display());

  Ok(())
}

/// Uploads the target's export and prints the shareable Thunderstore code.
pub async fn export_code(client: &ThunderstoreClient, target: &Target) -> AppResult<()> {
  let code = portability::export_code_in(&target.dir, target.name(), client).await?;

  println!("profile code: {code}");

  Ok(())
}

/// Imports mods into the target from an `.r2z` file, an existing r2modman profile
/// directory, or a profile code (Thunderstore's or Hexium's — see
/// [`fetch_profile_code`]).
///
/// `source` is classified by the engine's [`portability::ImportSource`], which
/// dispatches on what it names on disk and falls back to a code, so no flag is
/// needed to disambiguate and vmm decides nothing about the routing. A file or
/// code import reinstalls each mod at
/// its **latest** version (the engine's resolution is version-agnostic and the
/// export's pinned version is not honored), so the installed versions are printed
/// to make any divergence visible. Every mod name is resolved against `sources`'
/// merged manifest (see [`crate::sources::merged_manifest`]), the same
/// multi-source resolution `install`/`update` use, so a Hexium-only mod named
/// in the import is not invisible to it just because the export format itself
/// has no room to record which source it came from. A directory import copies
/// the profile as-is and downloads nothing, with one exception: any mod loader
/// the adopted `mods.yml` names that arrives without an install record is
/// reinstalled afterwards (also resolved against `sources`), so it gains the
/// record that makes it manageable and removable. That reinstall pulls the
/// loader's **latest** version, the same as a file or code import, so the
/// version the source profile had pinned is not preserved;
/// [`report_reinstalled_loaders`] discloses that. Regular mods are unaffected
/// and are still adopted at the versions the source had, and a source naming
/// no unrecorded loader stays entirely offline.
///
/// A directory import is a raw copy with no pre-clean: importing into a target
/// that already holds a different set of mods overwrites `mods.yml` but leaves
/// the previous mods' files on disk, now untracked and orphaned. Import into an
/// empty target, or accept that leftover.
///
/// A directory source that overlaps the target is refused up front by the
/// engine's `ensure_disjoint_trees`, called from inside `import_r2modman_dir`
/// before anything is copied.
///
/// A directory that does **not** contain a `mods.yml` (a live Gale profile,
/// which keeps its state in SQLite rather than a per-profile record) is
/// treated as a raw profile to adopt rather than falling through to the
/// engine's r2modman-dir route, which would silently adopt nothing: see
/// [`import_gale_dir`].
///
/// `prune: true` (the CLI default; `--additive` turns it off) additionally
/// uninstalls anything this target has that the import's source no longer
/// names, so the target converges on the source instead of only ever growing.
/// It does not apply to a directory source, which is a raw, non-reconciling
/// copy and ignores it.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn import(
  client: &ThunderstoreClient,
  eco: &Ecosystem,
  target: &Target,
  source: &str,
  prune: bool,
  sources: &[Box<dyn ModSource>],
) -> AppResult<()> {
  import_tracking(client, eco, target, source, prune, sources, false).await
}

/// [`import`], with `track_latest` (the config's root setting) choosing whether
/// the source's mod versions are respected or ignored in favour of latest.
///
/// A list source (`.r2z`, profile code, r2modman directory) names a version for
/// every mod. By default each is installed at exactly that version and
/// remembered as a pin (see [`crate::sources::state_file`]) so `update mods`
/// keeps it there; a pinned version no source offers is skipped with a warning
/// and the installed one is kept. With `track_latest` nothing is pinned and
/// everything goes to latest. A live Gale directory names no versions, so it
/// installs latest either way.
pub async fn import_tracking(
  client: &ThunderstoreClient,
  eco: &Ecosystem,
  target: &Target,
  source: &str,
  prune: bool,
  sources: &[Box<dyn ModSource>],
  track_latest: bool,
) -> AppResult<()> {
  let classified = ImportSource::classify(source);

  if let ImportSource::R2modmanDir(dir) = &classified {
    if !dir.join("mods.yml").exists() {
      return import_gale_dir(eco, target, dir, sources).await;
    }

    let adopted = import_r2modman_dir_with_sources(eco, target, dir, sources, track_latest).await?;

    super::report_installed(target, &adopted.adopted)?;
    report_reinstalled_loaders(&adopted.reinstalled, track_latest);

    return Ok(());
  }

  if prune {
    return import_with_prune(client, eco, target, &classified, sources, track_latest).await;
  }

  let zip_bytes = decode_archive_or_code(client, sources, &classified).await?;
  let (installed, source_map) =
    import_zip_with_sources(target, eco, sources, &zip_bytes, track_latest).await?;

  super::report_installed(target, &installed)?;
  record_resolved_sources(target, &installed, &source_map)?;
  note_latest_versions(track_latest);

  Ok(())
}

/// Tells the user, only when `track_latest` is why, that the source's versions
/// were not the ones installed.
fn note_latest_versions(track_latest: bool) {
  if track_latest {
    println!(
      "\nNote: track_latest is set, so each mod was installed at its latest version, not the listed one."
    );
  }
}

/// Records, in the sidecar, which configured source each of `installed`
/// resolved from during this import — the newly-resolved source
/// [`crate::sources::merged_manifest`] picked, not anything preserved from the
/// profile being imported (the interchange formats have no room for that; see
/// [`crate::sources::state_file`]'s module docs).
fn record_resolved_sources(
  target: &Target,
  installed: &[String],
  source_map: &HashMap<String, SourceId>,
) -> AppResult<()> {
  crate::sources::record_sources(
    target,
    installed
      .iter()
      .filter_map(|name| source_map.get(name).map(|id| (name.clone(), *id))),
  )
}

/// Decodes an `.r2z`'s bytes from an [`ImportSource::Archive`] or fetches and
/// decodes a [`ImportSource::Code`]'s payload, ready for [`portability::read_export`]
/// or [`portability::import_r2z_in`].
async fn decode_archive_or_code(
  client: &ThunderstoreClient,
  sources: &[Box<dyn ModSource>],
  classified: &ImportSource,
) -> AppResult<Vec<u8>> {
  match classified {
    ImportSource::Archive(path) => Ok(std::fs::read(path)?),
    ImportSource::Code(code) => {
      let body = fetch_profile_code(client, sources, code).await?;

      decode_profile_payload(&body)
    }
    ImportSource::R2modmanDir(_) => {
      unreachable!("directory sources are routed before this is ever called")
    }
  }
}

/// Fetches a profile code, trying Hexium first when it is a configured
/// source, falling back to `client` (Thunderstore) otherwise or when the
/// Hexium attempt fails.
///
/// Gale's own export logic uploads a profile to Hexium's `legacyprofile`
/// endpoint instead of Thunderstore's whenever the profile has a
/// Hexium-exclusive mod (Gale calls this "Hexium-only" and warns that other
/// tools can't import the resulting code, since they only ever ask
/// Thunderstore). Hexium mirrors that endpoint under the identical
/// `api/experimental/legacyprofile` path, just on its own `base_url`, so a
/// client pointed there resolves the same code Gale generated — there is no
/// other Gale-specific system to talk to. Hexium is tried first rather than
/// last because a user only opts into Hexium at all when they expect to use
/// it, and most Hexium-hosted codes exist precisely because they are not on
/// Thunderstore; a code that turns out to be Thunderstore-hosted anyway (the
/// common case even with Hexium enabled) still resolves through the
/// Thunderstore fallback below.
async fn fetch_profile_code(
  client: &ThunderstoreClient,
  sources: &[Box<dyn ModSource>],
  code: &str,
) -> AppResult<Vec<u8>> {
  let hexium_client = sources
    .iter()
    .find(|source| source.id() == SourceId::Hexium)
    .map(|source| source.download_client());

  if let Some(hexium_client) = hexium_client
    && let Ok(body) = hexium_client.fetch_profile_code(code).await
  {
    return Ok(body);
  }

  Ok(client.fetch_profile_code(code).await?)
}

/// Resolves `zip_bytes`' mod list against `sources`' merged manifest and installs
/// it, the multi-source counterpart of the engine's own `import_r2z_in` (which
/// resolves against a single client's manifest and always installs latest).
///
/// Unless `track_latest`, each listed mod is installed at the version the list
/// names and recorded as a pin. A listed mod whose version nothing offers is
/// skipped with a warning (it stays as installed) rather than failing the whole
/// import, which the engine's own helper would do. The rest mirrors that helper:
/// disabled mods stay disabled, any failure is an error, and the export's
/// per-mod configs are extracted last so they win over package defaults.
async fn import_zip_with_sources(
  target: &Target,
  eco: &Ecosystem,
  sources: &[Box<dyn ModSource>],
  zip_bytes: &[u8],
  track_latest: bool,
) -> AppResult<(Vec<String>, HashMap<String, SourceId>)> {
  let export = portability::read_export(zip_bytes)?;

  let pins: HashMap<String, String> = match track_latest {
    true => HashMap::new(),
    false => export
      .mods
      .iter()
      .map(|entry| (entry.name.clone(), entry.version.to_string()))
      .collect(),
  };

  let (index, source_map) =
    crate::sources::merged_manifest_pinned(&crate::sources::as_refs(sources), false, &pins).await?;
  let download_client = crate::sources::first_download_client(sources)?;

  let missing = crate::sources::unavailable_pins(&index, &pins);

  crate::sources::warn_unavailable_pins(&missing, &pins);

  let desired: Vec<String> = export
    .mods
    .iter()
    .map(|entry| entry.name.clone())
    .filter(|name| !missing.contains(name))
    .collect();
  let protect: Vec<String> = export
    .mods
    .iter()
    .filter(|entry| !entry.enabled)
    .map(|entry| entry.name.clone())
    .collect();
  let batch = profile::InstallBatch::new(desired, protect);

  let outcome = profile::install_batch(
    &target.dir,
    &target.base,
    eco,
    &index,
    download_client,
    GAME,
    &batch,
    modlist::now_millis(),
  )
  .await?;

  if let Some((name, error)) = outcome.failed.into_iter().next() {
    return Err(AppError::Other(format!("importing {name}: {error}")));
  }

  portability::extract_configs_in(&target.dir, zip_bytes)?;

  let mut installed = outcome.succeeded;

  installed.sort();
  installed.dedup();

  crate::sources::record_pins(
    target,
    pins
      .iter()
      .filter(|(name, _)| installed.contains(name))
      .map(|(name, version)| (name.clone(), version.clone())),
  )?;

  Ok((installed, source_map))
}

/// Copies an r2modman profile directory (one that already carries a `mods.yml`)
/// into the target, then reinstalls any mod loader it names that arrived
/// without an install record — the vmm-side counterpart of the engine's own
/// `adopt_r2modman_dir_in`, which resolves that reinstall against a single
/// client's manifest instead of `sources`' merged one.
///
/// Regular (non-loader) mods are never reinstalled here: they are adopted
/// as-is, at whatever version the source profile had, exactly like the
/// engine's own route.
async fn import_r2modman_dir_with_sources(
  eco: &Ecosystem,
  target: &Target,
  source_dir: &Path,
  sources: &[Box<dyn ModSource>],
  track_latest: bool,
) -> AppResult<portability::AdoptedProfile> {
  portability::import_r2modman_dir(source_dir, &target.dir)?;

  let entries = modlist::read(&target.dir)?;
  let adopted: Vec<String> = entries.iter().map(|entry| entry.name.clone()).collect();

  // The adopted `mods.yml` is a list with versions like any other, so its
  // versions become pins: otherwise the next `update mods` would move them.
  let pins: HashMap<String, String> = match track_latest {
    true => HashMap::new(),
    false => entries
      .iter()
      .map(|entry| (entry.name.clone(), entry.version_number.to_string()))
      .collect(),
  };

  crate::sources::record_pins(target, pins.clone())?;

  // Every recognised loader without a `_state` tracker, the same test
  // `adopt_r2modman_dir_in` uses: r2modman never wrote it, so an adopted
  // loader is otherwise unremovable (see `ensure_removable`).
  let unrecorded: Vec<String> = adopted
    .iter()
    .filter(|name| eco.modloader_package(name).is_some())
    .filter(|name| !thunderstore_engine::install::state_file_path(&target.dir, name).exists())
    .cloned()
    .collect();

  if unrecorded.is_empty() {
    let mut profile = portability::AdoptedProfile::default();
    profile.adopted = adopted;

    return Ok(profile);
  }

  let (index, source_map) =
    crate::sources::merged_manifest_pinned(&crate::sources::as_refs(sources), false, &pins).await?;
  let download_client = crate::sources::first_download_client(sources)?;

  // A loader whose recorded version nothing offers stays as adopted, unrecorded.
  let missing = crate::sources::unavailable_pins(&index, &pins);

  crate::sources::warn_unavailable_pins(&missing, &pins);

  let unrecorded: Vec<String> = unrecorded
    .into_iter()
    .filter(|name| !missing.contains(name))
    .collect();

  for full_name in &unrecorded {
    profile::install_mod_in(
      &target.dir,
      &target.base,
      eco,
      &index,
      download_client,
      GAME,
      full_name,
      modlist::now_millis(),
    )
    .await?;
  }

  record_resolved_sources(target, &unrecorded, &source_map)?;

  let mut profile = portability::AdoptedProfile::default();
  profile.adopted = adopted;
  profile.reinstalled = unrecorded;

  Ok(profile)
}

/// Adopts a live Gale profile directory (mod files on disk, but no `mods.yml`
/// — Gale keeps its live state in SQLite instead) as a fresh install: copies
/// the directory over, plans an uninstall of everything now sitting there
/// (which is, with no `mods.yml` to protect anything, every mod folder Gale
/// had installed), and reinstalls each one from the merged manifest so it
/// gains a real record.
///
/// Reinstalls **everything**, not just an unrecorded loader as the ordinary
/// r2modman-directory route does: nothing here can attribute a copied file to
/// a specific mod by shape, so every mod is adopted the same way a mod loader
/// already is. Every mod therefore lands at its **latest** version, not
/// necessarily the one Gale had pinned.
async fn import_gale_dir(
  eco: &Ecosystem,
  target: &Target,
  source_dir: &Path,
  sources: &[Box<dyn ModSource>],
) -> AppResult<()> {
  portability::ensure_disjoint_trees(source_dir, &target.dir)?;
  portability::import_r2modman_dir(source_dir, &target.dir)?;

  // With no `mods.yml` copied over (there was none to copy), every
  // `<Owner-Name>`-shaped folder the copy just placed is "untracked" against
  // an empty keep-set, which is exactly the set of mods Gale had installed.
  let batch = profile::plan_uninstall_batch(&target.dir, eco, GAME, None)?;
  let names = untracked_names(&batch.untracked);

  if names.is_empty() {
    println!(
      "adopted {} from {}; it named no mods to install",
      crate::target::describe(target),
      source_dir.display()
    );

    return Ok(());
  }

  let (index, source_map) =
    crate::sources::merged_manifest(&crate::sources::as_refs(sources), false).await?;
  let download_client = crate::sources::first_download_client(sources)?;

  let outcome = portability::adopt_names_in(
    &target.dir,
    &target.base,
    eco,
    &index,
    download_client,
    GAME,
    &names,
    modlist::now_millis(),
  )
  .await?;

  super::report_installed(target, &outcome.adopted)?;
  record_resolved_sources(target, &outcome.adopted, &source_map)?;

  for dir in &outcome.swept {
    println!("swept stale folder {}", dir.display());
  }

  if !outcome.remaining.is_empty() {
    return Err(unadopted_gale_dir_error(
      source_dir,
      &outcome.remaining,
      &outcome.failed,
    ));
  }

  println!(
    "\nadopted a Gale-style profile: every mod was reinstalled at its latest \
     version, not necessarily the one Gale had pinned."
  );

  Ok(())
}

/// Builds the error [`import_gale_dir`] returns when some of the mods it found
/// on disk could not be adopted, so the caller knows what is still stranded
/// there and that a reconciling command (e.g. `vmm uninstall`) would sweep it
/// as delisted before a retry finishes the job.
fn unadopted_gale_dir_error(
  source_dir: &Path,
  remaining: &[String],
  failed: &[(String, thunderstore_engine::error::Error)],
) -> AppError {
  let mut detail = format!(
    "These mods were not adopted: {}\n\nTheir files are still on disk, but \
     mods.yml does not record them, so a command that reconciles installs \
     would sweep them as delisted. Re-run the import to finish adopting them.",
    remaining.join(", ")
  );

  if !failed.is_empty() {
    let reasons = failed
      .iter()
      .map(|(name, error)| format!("{name}: {error}"))
      .collect::<Vec<_>>()
      .join("; ");

    detail.push_str(&format!("\n\nKnown reasons: {reasons}"));
  }

  let suggestion = format!("vmm import {}", source_dir.display());

  AppError::advice(
    "adopting the Gale profile stopped part way through.",
    detail,
    &[suggestion.as_str()],
  )
}

/// The `<Owner-Name>` mod identifiers named by `untracked`'s root-relative
/// paths (e.g. `BepInEx/plugins/Owner-Name`), taking the final path
/// component.
fn untracked_names(untracked: &[PathBuf]) -> Vec<String> {
  untracked
    .iter()
    .filter_map(|path| path.file_name())
    .map(|name| name.to_string_lossy().to_string())
    .collect()
}

/// Imports an archive or profile-code `classified` source (never a directory —
/// that route is handled before this is ever called), then uninstalls whatever
/// this target has that the source no longer names.
async fn import_with_prune(
  client: &ThunderstoreClient,
  eco: &Ecosystem,
  target: &Target,
  classified: &ImportSource,
  sources: &[Box<dyn ModSource>],
  track_latest: bool,
) -> AppResult<()> {
  let zip_bytes = decode_archive_or_code(client, sources, classified).await?;

  reconcile_zip(eco, target, sources, &zip_bytes, track_latest).await
}

/// Installs `zip_bytes`' modlist, then uninstalls whatever this target has that
/// it no longer names: the shared core of `import` (pruning) and `sync`.
///
/// The export is decoded once and its full mod list (`wanted`) is read from
/// that same decode, so `stale` is computed against what the source actually
/// names, not merely what happened to install successfully.
pub async fn reconcile_zip(
  eco: &Ecosystem,
  target: &Target,
  sources: &[Box<dyn ModSource>],
  zip_bytes: &[u8],
  track_latest: bool,
) -> AppResult<()> {
  let previous: HashSet<String> = modlist::read(&target.dir)
    .unwrap_or_default()
    .into_iter()
    .map(|entry| entry.name)
    .collect();

  let export = portability::read_export(zip_bytes)?;
  let wanted: HashSet<String> = export.mods.into_iter().map(|entry| entry.name).collect();

  // Checked before anything is installed or removed: an empty source (a
  // profile someone blanked out or a truncated export) would otherwise
  // uninstall everything, including the mod loader.
  if wanted.is_empty() && !previous.is_empty() {
    return Err(AppError::advice(
      "refusing to prune: the source names no mods.",
      format!(
        "Reconciling against an empty modlist would uninstall all {} installed \
         mod(s). Nothing was changed.",
        previous.len()
      ),
      &["vmm import --additive <source>"],
    ));
  }

  let (installed, source_map) =
    import_zip_with_sources(target, eco, sources, zip_bytes, track_latest).await?;

  super::report_installed(target, &installed)?;
  record_resolved_sources(target, &installed, &source_map)?;
  note_latest_versions(track_latest);

  let stale: Vec<String> = previous.difference(&wanted).cloned().collect();

  if stale.is_empty() {
    println!("\nnothing to prune: every previously-installed mod is still named.");

    return Ok(());
  }

  let blocked = profile::check_removable_batch(&target.dir, eco, &stale);
  let blocked_names: HashSet<&str> = blocked.iter().map(|(name, _)| name.as_str()).collect();

  for (name, refusal) in &blocked {
    println!("\ncould not prune {name}: {refusal}");
  }

  let removable: Vec<String> = stale
    .into_iter()
    .filter(|name| !blocked_names.contains(name.as_str()))
    .collect();

  if removable.is_empty() {
    return Ok(());
  }

  let uninstall_batch = profile::plan_uninstall_batch(&target.dir, eco, GAME, Some(&removable))?;
  let uninstall_outcome = profile::uninstall_batch(&target.dir, eco, GAME, &uninstall_batch)?;

  for name in &uninstall_outcome.succeeded {
    println!("pruned {name}");
  }

  let keep: HashSet<String> = modlist::read(&target.dir)
    .unwrap_or_default()
    .into_iter()
    .map(|entry| entry.name)
    .collect();

  crate::sources::prune_sources(target, &keep)?;

  super::report_batch_failures(&uninstall_outcome)
}

/// Unwraps a profile-code response body into `.r2z` bytes: validates the
/// `#r2modman` prefix, then base64-decodes the remainder.
///
/// A five-line reimplementation of the engine's private
/// `decode_profile_payload`, safe to duplicate because this is the
/// r2modman/Thunderstore ecosystem's public interchange format, not an
/// engine-internal detail.
fn decode_profile_payload(body: &[u8]) -> AppResult<Vec<u8>> {
  let text = std::str::from_utf8(body)
    .map_err(|e| AppError::Other(format!("profile code is not valid UTF-8: {e}")))?;
  let encoded = text
    .strip_prefix("#r2modman")
    .ok_or_else(|| AppError::Other("profile code is missing its #r2modman prefix".to_string()))?;

  base64::engine::general_purpose::STANDARD
    .decode(encoded.trim())
    .map_err(|e| AppError::Other(format!("decoding profile code: {e}")))
}

/// Discloses that an adopted mod loader was reinstalled at its latest version.
///
/// The engine has to reinstall a loader to get an exact file list for it, since
/// a copied `BepInEx/core` file cannot be attributed by shape (see
/// [`portability::adopt_r2modman_dir_in`]), and that pulls the latest version
/// rather than the one the source profile recorded. Saying so is the point:
/// everything else about a directory import is adopted as-is, which sets an
/// expectation this one step breaks. Printed per loader, because the loader
/// registry is keyed on package identity rather than the game, so nothing rules
/// out more than one.
fn report_reinstalled_loaders(reinstalled: &[String], track_latest: bool) {
  for loader in reinstalled {
    let version = match track_latest {
      true => "at its latest version; the version your source profile recorded is not preserved",
      false => "at the version your source profile recorded",
    };

    println!("\nreinstalled {loader} {version} so it can be managed and uninstalled");
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::test_support::Fixture;
  use thunderstore_engine::profile::modlist;
  use tokio::runtime::Runtime;

  /// A `mods.yml` naming `denikson-BepInExPack_Valheim` at 5.4.2200, shaped the
  /// way r2modman actually leaves one on disk.
  ///
  /// Matches the shape `modlist::write` produces rather than a minimal guess:
  /// `ProfileMod` has no `#[serde(default)]` on most fields, so a sparser
  /// document fails to parse. The shape was confirmed by generating a
  /// reference file with `modlist::write` in a scratch test and reading back
  /// what it wrote.
  fn adopted_loader_mods_yml() -> &'static str {
    "- manifestVersion: 1\n  name: denikson-BepInExPack_Valheim\n  \
     authorName: denikson\n  websiteUrl: ''\n  displayName: BepInExPack_Valheim\n  \
     description: A mod\n  gameVersion: '0'\n  networkMode: both\n  \
     packageType: other\n  installMode: managed\n  installedAtTime: 1700000000000\n  \
     loaders: []\n  dependencies: []\n  incompatibilities: []\n  \
     optionalDependencies: []\n  versionNumber: {major: 5, minor: 4, patch: 2200}\n  \
     enabled: true\n  onlineSource: true\n  trustedPackage: false\n"
  }

  #[test]
  fn export_writes_an_r2z_into_the_exports_dir() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    export_file(&target).unwrap();

    let exports: Vec<_> = std::fs::read_dir(target.exports_dir())
      .unwrap()
      .flatten()
      .map(|entry| entry.file_name().to_string_lossy().to_string())
      .collect();

    assert_eq!(exports.len(), 1);
    assert!(exports[0].starts_with("default_"), "got: {:?}", exports[0]);
    assert!(exports[0].ends_with(".r2z"));
  }

  #[test]
  fn an_export_imports_into_another_target() {
    let fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &source,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    export_file(&source).unwrap();

    let archive = std::fs::read_dir(source.exports_dir())
      .unwrap()
      .flatten()
      .next()
      .unwrap()
      .path();

    let destination = fixture.profile_target("imported");

    runtime
      .block_on(import(
        &fixture.client,
        &eco,
        &destination,
        archive.to_str().unwrap(),
        false,
        &fixture.sources(),
      ))
      .unwrap();

    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Owner-ModA");
    assert!(
      destination
        .dir
        .join("BepInEx/plugins/Owner-ModA/ModA.dll")
        .exists()
    );
  }

  #[test]
  fn importing_a_profile_code_installs_the_shared_mods() {
    let mut fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &source,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    export_file(&source).unwrap();

    let archive = std::fs::read_dir(source.exports_dir())
      .unwrap()
      .flatten()
      .next()
      .unwrap()
      .path();
    let zip_bytes = std::fs::read(&archive).unwrap();
    let payload = format!(
      "#r2modman\n{}",
      base64::engine::general_purpose::STANDARD.encode(zip_bytes)
    );

    fixture
      .server
      .mock("GET", "/api/experimental/legacyprofile/get/shared-code/")
      .with_status(200)
      .with_body(payload)
      .create();

    // `fixture.client` only overrides `package_index_url`, so its `base_url`
    // is still the real Thunderstore host. The profile-code endpoints are
    // built from `base_url`, so a client for this path must point it at the
    // mock server too.
    let url = fixture.server.url();
    let index_url = format!("{url}/pkg/");
    let client = ThunderstoreClient::builder()
      .base_url(url)
      .package_index_url(index_url)
      .cache_dir(fixture.base.path())
      .build()
      .unwrap();

    let destination = fixture.profile_target("shared");

    runtime
      .block_on(import(
        &client,
        &eco,
        &destination,
        "shared-code",
        false,
        &fixture.sources(),
      ))
      .unwrap();

    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Owner-ModA");
    assert!(
      destination
        .dir
        .join("BepInEx/plugins/Owner-ModA/ModA.dll")
        .exists()
    );
  }

  #[test]
  fn importing_a_profile_code_prefers_hexium_when_it_is_configured() {
    let mut fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &source,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    export_file(&source).unwrap();

    let archive = std::fs::read_dir(source.exports_dir())
      .unwrap()
      .flatten()
      .next()
      .unwrap()
      .path();
    let zip_bytes = std::fs::read(&archive).unwrap();
    let payload = format!(
      "#r2modman\n{}",
      base64::engine::general_purpose::STANDARD.encode(zip_bytes)
    );

    // The code lives only on the Hexium mock: Thunderstore's mock (the
    // `client` below points there) never gets a mock for this path, so if
    // the fallback wrongly asked Thunderstore first, or never asked Hexium at
    // all, this would fail rather than silently succeeding.
    fixture
      .hexium_server
      .mock("GET", "/api/experimental/legacyprofile/get/hexium-code/")
      .with_status(200)
      .with_body(payload)
      .create();

    let url = fixture.server.url();
    let index_url = format!("{url}/pkg/");
    let client = ThunderstoreClient::builder()
      .base_url(url)
      .package_index_url(index_url)
      .cache_dir(fixture.base.path())
      .build()
      .unwrap();

    let destination = fixture.profile_target("hexium-preferred");

    runtime
      .block_on(import(
        &client,
        &eco,
        &destination,
        "hexium-code",
        false,
        &fixture.multi_sources(),
      ))
      .unwrap();

    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Owner-ModA");
  }

  #[test]
  fn importing_a_profile_code_falls_back_to_thunderstore_when_hexium_lacks_it() {
    let mut fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &source,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    export_file(&source).unwrap();

    let archive = std::fs::read_dir(source.exports_dir())
      .unwrap()
      .flatten()
      .next()
      .unwrap()
      .path();
    let zip_bytes = std::fs::read(&archive).unwrap();
    let payload = format!(
      "#r2modman\n{}",
      base64::engine::general_purpose::STANDARD.encode(zip_bytes)
    );

    // Hexium is configured but does not have this code (the common case: most
    // profiles are still Thunderstore-hosted even once Hexium is enabled), so
    // the import must fall through to Thunderstore rather than failing.
    fixture
      .hexium_server
      .mock(
        "GET",
        "/api/experimental/legacyprofile/get/thunderstore-code/",
      )
      .with_status(404)
      .create();

    fixture
      .server
      .mock(
        "GET",
        "/api/experimental/legacyprofile/get/thunderstore-code/",
      )
      .with_status(200)
      .with_body(payload)
      .create();

    let url = fixture.server.url();
    let index_url = format!("{url}/pkg/");
    let client = ThunderstoreClient::builder()
      .base_url(url)
      .package_index_url(index_url)
      .cache_dir(fixture.base.path())
      .build()
      .unwrap();

    let destination = fixture.profile_target("thunderstore-fallback");

    runtime
      .block_on(import(
        &client,
        &eco,
        &destination,
        "thunderstore-code",
        false,
        &fixture.multi_sources(),
      ))
      .unwrap();

    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Owner-ModA");
  }

  #[test]
  fn importing_the_target_directory_itself_is_refused_before_anything_is_copied() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    // Stands in for the files a game-dir-mode import would destroy: the loader
    // proxy beside the executable and a hand-edited config.
    let proxy = target.dir.join("winhttp.dll");
    let config = target.dir.join("BepInEx/config/mod.cfg");

    std::fs::write(&proxy, b"loader-proxy-bytes").unwrap();
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, b"hand-tuned = true").unwrap();

    let result = runtime.block_on(import(
      &fixture.client,
      &eco,
      &target,
      target.dir.to_str().unwrap(),
      false,
      &[],
    ));

    assert!(
      result.is_err(),
      "importing the install root into itself must be refused"
    );
    // `fs::copy(p, p)` opens the destination with `O_TRUNC` and returns Ok, so a
    // zero-length file here is the signature of the bug this guards.
    for path in [
      &proxy,
      &config,
      &target.dir.join("BepInEx/plugins/Owner-ModA/ModA.dll"),
      &target.mods_yml(),
    ] {
      let size = std::fs::metadata(path).unwrap().len();

      assert!(
        size > 0,
        "{} was truncated to {} bytes",
        path.display(),
        size
      );
    }
  }

  #[test]
  fn importing_a_directory_adopts_it_as_is() {
    let fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &source,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    let destination = fixture.profile_target("adopted");

    // A loader-less directory import (this source names only Owner-ModA) must
    // never touch the network. A fresh server (not `fixture.server`, whose
    // mocks are permissive and would mask a regression) carries
    // zero-expectation mocks on the endpoints a `.r2z` or code import would
    // hit, so `.assert()` fails loudly if either is called.
    let mut trap = mockito::Server::new();
    let no_manifest = trap.mock("GET", "/pkg/").expect(0).create();
    let no_download = trap.mock("GET", "/dl/ModA.zip").expect(0).create();
    let trap_client = ThunderstoreClient::builder()
      .package_index_url(format!("{}/pkg/", trap.url()))
      .cache_dir(fixture.base.path())
      .build()
      .unwrap();

    Runtime::new()
      .unwrap()
      .block_on(import(
        &trap_client,
        &eco,
        &destination,
        source.dir.to_str().unwrap(),
        false,
        &[],
      ))
      .unwrap();

    no_manifest.assert();
    no_download.assert();

    // A loader-less directory import is a copy, not a reinstall.
    assert_eq!(modlist::read(&destination.dir).unwrap().len(), 1);
    assert!(
      destination
        .dir
        .join("BepInEx/plugins/Owner-ModA/ModA.dll")
        .exists()
    );
  }

  #[test]
  fn a_directory_import_gives_the_loader_an_install_record() {
    let fixture = Fixture::new();
    let target = fixture.profile_target("destination");
    let eco = Ecosystem::bundled();

    // A source profile shaped the way r2modman leaves one: a mods.yml naming
    // the loader, the loader's files on disk, and no `_state` tracker, because
    // `_state` is this engine's own invention.
    let source = tempfile::TempDir::new().unwrap();

    std::fs::write(source.path().join("mods.yml"), adopted_loader_mods_yml()).unwrap();
    std::fs::create_dir_all(source.path().join("BepInEx/core")).unwrap();
    std::fs::write(source.path().join("winhttp.dll"), b"proxy").unwrap();

    Runtime::new()
      .unwrap()
      .block_on(import(
        &fixture.client,
        &eco,
        &target,
        source.path().to_str().unwrap(),
        false,
        &fixture.sources(),
      ))
      .unwrap();

    // The loader now has an install record, so uninstalling it can remove its
    // files exactly rather than orphaning them.
    assert!(
      thunderstore_engine::install::state_file_path(&target.dir, "denikson-BepInExPack_Valheim")
        .exists(),
      "the adopted loader must end up with a _state tracker"
    );
  }

  #[test]
  fn reinstalling_the_loader_is_skipped_when_it_already_has_a_state_tracker() {
    let fixture = Fixture::new();
    let destination = fixture.profile_target("already-installed");
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    // The destination already has the loader properly installed through the
    // normal pipeline, so it already carries a `_state` tracker before the
    // directory import runs.
    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &destination,
        &["denikson-BepInExPack_Valheim".to_string()],
      ))
      .unwrap();
    assert!(
      thunderstore_engine::install::state_file_path(
        &destination.dir,
        "denikson-BepInExPack_Valheim"
      )
      .exists()
    );

    // A source profile that also names the loader, r2modman-shaped as above.
    let source = tempfile::TempDir::new().unwrap();

    std::fs::write(source.path().join("mods.yml"), adopted_loader_mods_yml()).unwrap();
    std::fs::create_dir_all(source.path().join("BepInEx/core")).unwrap();
    std::fs::write(source.path().join("winhttp.dll"), b"proxy").unwrap();

    // A fresh server with zero-expectation mocks on the endpoints a reinstall
    // would hit, so `.assert()` fails loudly if the already-tracked loader is
    // reinstalled anyway. Matches the pattern in
    // `importing_a_directory_adopts_it_as_is`.
    let mut trap = mockito::Server::new();
    let no_manifest = trap.mock("GET", "/pkg/").expect(0).create();
    let no_download = trap
      .mock("GET", "/dl/BepInExPack_Valheim.zip")
      .expect(0)
      .create();
    let trap_client = ThunderstoreClient::builder()
      .package_index_url(format!("{}/pkg/", trap.url()))
      .cache_dir(fixture.base.path())
      .build()
      .unwrap();

    runtime
      .block_on(import(
        &trap_client,
        &eco,
        &destination,
        source.path().to_str().unwrap(),
        false,
        &[],
      ))
      .unwrap();

    no_manifest.assert();
    no_download.assert();
  }

  #[test]
  fn importing_a_gale_style_directory_with_no_mods_yml_adopts_and_reinstalls_everything() {
    let fixture = Fixture::new();
    let destination = fixture.profile_target("from-gale");
    let eco = Ecosystem::bundled();

    // A live Gale profile: mod files on disk under the namespaced routes, but no
    // `mods.yml` at all, since Gale keeps its live state in SQLite rather than a
    // per-profile record.
    let source = tempfile::TempDir::new().unwrap();

    std::fs::create_dir_all(source.path().join("BepInEx/plugins/Owner-ModA")).unwrap();
    std::fs::write(
      source.path().join("BepInEx/plugins/Owner-ModA/ModA.dll"),
      b"stale-copy",
    )
    .unwrap();

    Runtime::new()
      .unwrap()
      .block_on(import(
        &fixture.client,
        &eco,
        &destination,
        source.path().to_str().unwrap(),
        false,
        &fixture.sources(),
      ))
      .unwrap();

    // Reinstalled (not merely copied) from the manifest, so it now carries a
    // real record.
    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Owner-ModA");
    assert_eq!(
      crate::sources::read_sources(&destination).get("Owner-ModA"),
      Some(&crate::sources::SourceId::Thunderstore)
    );
  }

  #[test]
  fn importing_a_gale_style_directory_can_pull_a_hexium_only_mod() {
    let fixture = Fixture::new();
    let destination = fixture.profile_target("from-gale-hexium");
    let eco = Ecosystem::bundled();

    let source = tempfile::TempDir::new().unwrap();

    std::fs::create_dir_all(source.path().join("BepInEx/plugins/Hexium-OnlyMod")).unwrap();
    std::fs::write(
      source
        .path()
        .join("BepInEx/plugins/Hexium-OnlyMod/OnlyMod.dll"),
      b"stale-copy",
    )
    .unwrap();

    Runtime::new()
      .unwrap()
      .block_on(import(
        &fixture.client,
        &eco,
        &destination,
        source.path().to_str().unwrap(),
        false,
        &fixture.multi_sources(),
      ))
      .unwrap();

    assert_eq!(
      crate::sources::read_sources(&destination).get("Hexium-OnlyMod"),
      Some(&crate::sources::SourceId::Hexium)
    );
  }

  #[test]
  fn prune_is_ignored_for_a_directory_source() {
    let fixture = Fixture::new();
    let destination = fixture.profile_target("prune-ignored-for-dirs");
    let eco = Ecosystem::bundled();
    let source = tempfile::TempDir::new().unwrap();

    std::fs::write(source.path().join("mods.yml"), "[]").unwrap();

    Runtime::new()
      .unwrap()
      .block_on(import(
        &fixture.client,
        &eco,
        &destination,
        source.path().to_str().unwrap(),
        true,
        &[],
      ))
      .unwrap();
  }

  #[test]
  fn reconciling_against_an_empty_modlist_is_refused_and_changes_nothing() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    let mut buf = std::io::Cursor::new(Vec::new());
    {
      use std::io::Write;
      let mut zip = zip::ZipWriter::new(&mut buf);
      zip
        .start_file("export.r2x", zip::write::SimpleFileOptions::default())
        .unwrap();
      zip.write_all(b"profileName: Empty\nmods: []\n").unwrap();
      zip.finish().unwrap();
    }

    let message = runtime
      .block_on(reconcile_zip(&eco, &target, &[], buf.get_ref(), false))
      .unwrap_err()
      .to_string();

    assert!(message.contains("refusing to prune"), "got: {message}");
    assert!(target.dir.join("BepInEx/plugins/Owner-ModA").exists());
  }

  #[test]
  fn prune_uninstalls_a_mod_the_reimported_code_no_longer_names() {
    let mut fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &source,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    export_file(&source).unwrap();

    let archive = std::fs::read_dir(source.exports_dir())
      .unwrap()
      .flatten()
      .next()
      .unwrap()
      .path();
    let zip_bytes = std::fs::read(&archive).unwrap();
    let payload = format!(
      "#r2modman\n{}",
      base64::engine::general_purpose::STANDARD.encode(zip_bytes)
    );

    fixture
      .server
      .mock("GET", "/api/experimental/legacyprofile/get/shrinking-code/")
      .with_status(200)
      .with_body(payload)
      .create();

    let url = fixture.server.url();
    let index_url = format!("{url}/pkg/");
    let client = ThunderstoreClient::builder()
      .base_url(url)
      .package_index_url(index_url)
      .cache_dir(fixture.base.path())
      .build()
      .unwrap();

    let destination = fixture.profile_target("shrinking-target");

    // The destination already has an extra mod the shared code does not name.
    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &destination,
        &["Owner-ModB".to_string()],
      ))
      .unwrap();

    runtime
      .block_on(import(
        &client,
        &eco,
        &destination,
        "shrinking-code",
        true,
        &fixture.sources(),
      ))
      .unwrap();

    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Owner-ModA");
    assert!(!destination.dir.join("BepInEx/plugins/Owner-ModB").exists());
  }

  #[test]
  fn without_prune_a_reimport_leaves_a_dropped_mod_installed() {
    let mut fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &source,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    export_file(&source).unwrap();

    let archive = std::fs::read_dir(source.exports_dir())
      .unwrap()
      .flatten()
      .next()
      .unwrap()
      .path();
    let zip_bytes = std::fs::read(&archive).unwrap();
    let payload = format!(
      "#r2modman\n{}",
      base64::engine::general_purpose::STANDARD.encode(zip_bytes)
    );

    fixture
      .server
      .mock(
        "GET",
        "/api/experimental/legacyprofile/get/non-pruning-code/",
      )
      .with_status(200)
      .with_body(payload)
      .create();

    let url = fixture.server.url();
    let index_url = format!("{url}/pkg/");
    let client = ThunderstoreClient::builder()
      .base_url(url)
      .package_index_url(index_url)
      .cache_dir(fixture.base.path())
      .build()
      .unwrap();

    let destination = fixture.profile_target("non-pruning-target");

    runtime
      .block_on(crate::commands::install::run(
        &fixture.client,
        &eco,
        &destination,
        &["Owner-ModB".to_string()],
      ))
      .unwrap();

    // The default (`--additive`) add-only behavior must not silently change:
    // pinning this down means a future change can't flip it by accident.
    runtime
      .block_on(import(
        &client,
        &eco,
        &destination,
        "non-pruning-code",
        false,
        &fixture.sources(),
      ))
      .unwrap();

    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 2);
    assert!(modlist::find(&mods, "Owner-ModA").is_some());
    assert!(modlist::find(&mods, "Owner-ModB").is_some());
  }

  #[test]
  fn an_archive_import_resolves_a_hexium_only_mod_against_every_configured_source() {
    let fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    // Installed through the multi-source pipeline, so the export names a mod
    // that only exists on Hexium.
    runtime
      .block_on(crate::commands::install::run_with_sources(
        &fixture.multi_sources(),
        &eco,
        &source,
        &["Hexium-OnlyMod".to_string()],
        false,
      ))
      .unwrap();

    export_file(&source).unwrap();

    let archive = std::fs::read_dir(source.exports_dir())
      .unwrap()
      .flatten()
      .next()
      .unwrap()
      .path();

    let destination = fixture.profile_target("imported-hexium");

    // Importing against a Thunderstore-only `sources` would leave
    // `Hexium-OnlyMod` unresolvable; passing every configured source is what
    // makes this succeed at all.
    runtime
      .block_on(import(
        &fixture.client,
        &eco,
        &destination,
        archive.to_str().unwrap(),
        false,
        &fixture.multi_sources(),
      ))
      .unwrap();

    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Hexium-OnlyMod");
    assert_eq!(
      crate::sources::read_sources(&destination).get("Hexium-OnlyMod"),
      Some(&crate::sources::SourceId::Hexium)
    );
  }

  #[test]
  fn a_profile_code_import_resolves_a_hexium_only_mod_against_every_configured_source() {
    let mut fixture = Fixture::new();
    let source = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(crate::commands::install::run_with_sources(
        &fixture.multi_sources(),
        &eco,
        &source,
        &["Hexium-OnlyMod".to_string()],
        false,
      ))
      .unwrap();

    export_file(&source).unwrap();

    let archive = std::fs::read_dir(source.exports_dir())
      .unwrap()
      .flatten()
      .next()
      .unwrap()
      .path();
    let zip_bytes = std::fs::read(&archive).unwrap();
    let payload = format!(
      "#r2modman\n{}",
      base64::engine::general_purpose::STANDARD.encode(zip_bytes)
    );

    fixture
      .server
      .mock(
        "GET",
        "/api/experimental/legacyprofile/get/hexium-shared-code/",
      )
      .with_status(200)
      .with_body(payload)
      .create();

    let url = fixture.server.url();
    let index_url = format!("{url}/pkg/");
    let client = ThunderstoreClient::builder()
      .base_url(url)
      .package_index_url(index_url)
      .cache_dir(fixture.base.path())
      .build()
      .unwrap();

    let destination = fixture.profile_target("shared-hexium");

    runtime
      .block_on(import(
        &client,
        &eco,
        &destination,
        "hexium-shared-code",
        false,
        &fixture.multi_sources(),
      ))
      .unwrap();

    let mods = modlist::read(&destination.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Hexium-OnlyMod");
    assert_eq!(
      crate::sources::read_sources(&destination).get("Hexium-OnlyMod"),
      Some(&crate::sources::SourceId::Hexium)
    );
  }

  /// An `.r2z` naming each `(name, "x.y.z")` as an enabled mod.
  fn export_zip_with_versions(mods: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;

    let mut manifest = String::from("profileName: Pinned\nmods:\n");

    for (name, version) in mods {
      let parts: Vec<&str> = version.split('.').collect();

      manifest.push_str(&format!(
        "- name: {name}\n  version:\n    major: {}\n    minor: {}\n    patch: {}\n  enabled: true\n",
        parts[0], parts[1], parts[2]
      ));
    }

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

  fn installed_version(target: &Target, name: &str) -> Option<String> {
    modlist::read(&target.dir)
      .unwrap()
      .into_iter()
      .find(|entry| entry.name == name)
      .map(|entry| entry.version_number.to_string())
  }

  #[test]
  fn a_list_import_installs_the_listed_version_and_pins_it() {
    let fixture = Fixture::new();
    let (_server, sources) = fixture.multi_version_source();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let zip = export_zip_with_versions(&[("Owner-Multi", "1.0.0"), ("Owner-Steady", "1.0.0")]);

    Runtime::new()
      .unwrap()
      .block_on(import_zip_with_sources(
        &target, &eco, &sources, &zip, false,
      ))
      .unwrap();

    assert_eq!(
      installed_version(&target, "Owner-Multi").as_deref(),
      Some("1.0.0")
    );
    assert_eq!(
      crate::sources::read_pins(&target)
        .get("Owner-Multi")
        .map(String::as_str),
      Some("1.0.0")
    );
  }

  #[test]
  fn track_latest_ignores_the_listed_version_and_records_no_pin() {
    let fixture = Fixture::new();
    let (_server, sources) = fixture.multi_version_source();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let zip = export_zip_with_versions(&[("Owner-Multi", "1.0.0")]);

    Runtime::new()
      .unwrap()
      .block_on(import_zip_with_sources(&target, &eco, &sources, &zip, true))
      .unwrap();

    assert_eq!(
      installed_version(&target, "Owner-Multi").as_deref(),
      Some("2.0.0")
    );
    assert!(crate::sources::read_pins(&target).is_empty());
  }

  #[test]
  fn a_listed_version_no_source_offers_is_skipped_and_the_rest_still_install() {
    let fixture = Fixture::new();
    let (_server, sources) = fixture.multi_version_source();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let zip = export_zip_with_versions(&[("Owner-Multi", "9.9.9"), ("Owner-Steady", "1.0.0")]);

    Runtime::new()
      .unwrap()
      .block_on(import_zip_with_sources(
        &target, &eco, &sources, &zip, false,
      ))
      .unwrap();

    assert_eq!(installed_version(&target, "Owner-Multi"), None);
    assert_eq!(
      installed_version(&target, "Owner-Steady").as_deref(),
      Some("1.0.0")
    );
    assert!(!crate::sources::read_pins(&target).contains_key("Owner-Multi"));
  }

  #[test]
  fn an_unavailable_pin_leaves_the_installed_version_alone() {
    let fixture = Fixture::new();
    let (_server, sources) = fixture.multi_version_source();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(import_zip_with_sources(
        &target,
        &eco,
        &sources,
        &export_zip_with_versions(&[("Owner-Multi", "1.0.0")]),
        false,
      ))
      .unwrap();

    // Re-listing it at a version nothing offers must not move it to latest.
    runtime
      .block_on(import_zip_with_sources(
        &target,
        &eco,
        &sources,
        &export_zip_with_versions(&[("Owner-Multi", "9.9.9")]),
        false,
      ))
      .unwrap();

    assert_eq!(
      installed_version(&target, "Owner-Multi").as_deref(),
      Some("1.0.0")
    );
  }

  #[test]
  fn update_mods_keeps_a_pinned_version_and_track_latest_moves_it() {
    let fixture = Fixture::new();
    let (_server, sources) = fixture.multi_version_source();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(import_zip_with_sources(
        &target,
        &eco,
        &sources,
        &export_zip_with_versions(&[("Owner-Multi", "1.0.0")]),
        false,
      ))
      .unwrap();

    runtime
      .block_on(crate::commands::update::run_mods_with_sources(
        &sources, &eco, &target, false,
      ))
      .unwrap();

    assert_eq!(
      installed_version(&target, "Owner-Multi").as_deref(),
      Some("1.0.0")
    );

    runtime
      .block_on(crate::commands::update::run_mods_with_sources(
        &sources, &eco, &target, true,
      ))
      .unwrap();

    assert_eq!(
      installed_version(&target, "Owner-Multi").as_deref(),
      Some("2.0.0")
    );
  }

  #[test]
  fn installing_a_mod_by_hand_drops_its_pin_and_takes_latest() {
    let fixture = Fixture::new();
    let (_server, sources) = fixture.multi_version_source();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(import_zip_with_sources(
        &target,
        &eco,
        &sources,
        &export_zip_with_versions(&[("Owner-Multi", "1.0.0"), ("Owner-Steady", "1.0.0")]),
        false,
      ))
      .unwrap();

    runtime
      .block_on(crate::commands::install::run_with_sources(
        &sources,
        &eco,
        &target,
        &["Owner-Multi".to_string()],
        false,
      ))
      .unwrap();

    assert_eq!(
      installed_version(&target, "Owner-Multi").as_deref(),
      Some("2.0.0")
    );

    let pins = crate::sources::read_pins(&target);

    assert!(!pins.contains_key("Owner-Multi"));
    assert!(
      pins.contains_key("Owner-Steady"),
      "other pins are untouched"
    );
  }

  #[test]
  fn pruning_a_mod_drops_its_pin() {
    let fixture = Fixture::new();
    let (_server, sources) = fixture.multi_version_source();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(reconcile_zip(
        &eco,
        &target,
        &sources,
        &export_zip_with_versions(&[("Owner-Multi", "1.0.0"), ("Owner-Steady", "1.0.0")]),
        false,
      ))
      .unwrap();
    runtime
      .block_on(reconcile_zip(
        &eco,
        &target,
        &sources,
        &export_zip_with_versions(&[("Owner-Steady", "1.0.0")]),
        false,
      ))
      .unwrap();

    let pins = crate::sources::read_pins(&target);

    assert!(!pins.contains_key("Owner-Multi"));
    assert!(pins.contains_key("Owner-Steady"));
  }

  #[test]
  fn a_source_offering_the_pinned_version_beats_a_more_recent_one_that_does_not() {
    let fixture = Fixture::new();
    let sources = fixture.multi_sources();
    let pins = HashMap::from([("Owner-Shared".to_string(), "1.0.0".to_string())]);

    // Hexium's Owner-Shared is newer (2.0.0), Thunderstore's is the 1.0.0 the
    // pin asks for: recency alone would pick Hexium.
    let (index, map) = Runtime::new()
      .unwrap()
      .block_on(crate::sources::merged_manifest_pinned(
        &crate::sources::as_refs(&sources),
        false,
        &pins,
      ))
      .unwrap();

    assert_eq!(map.get("Owner-Shared"), Some(&SourceId::Thunderstore));
    assert!(crate::sources::unavailable_pins(&index, &pins).is_empty());
  }

  #[test]
  fn a_directory_import_pins_the_versions_its_mods_yml_records() {
    let fixture = Fixture::new();
    let (_server, sources) = fixture.multi_version_source();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    // Build a real mods.yml by importing at 1.0.0 into one target, then adopt
    // that directory into another.
    let origin = fixture.target();

    runtime
      .block_on(import_zip_with_sources(
        &origin,
        &eco,
        &sources,
        &export_zip_with_versions(&[("Owner-Multi", "1.0.0")]),
        false,
      ))
      .unwrap();

    let destination = fixture.profile_target("adopting");

    runtime
      .block_on(import_r2modman_dir_with_sources(
        &eco,
        &destination,
        &origin.dir,
        &sources,
        false,
      ))
      .unwrap();

    assert_eq!(
      crate::sources::read_pins(&destination)
        .get("Owner-Multi")
        .map(String::as_str),
      Some("1.0.0")
    );
  }
}
