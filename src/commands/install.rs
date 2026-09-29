use crate::error::AppResult;
use crate::sources::ModSource;
use crate::target::{GAME, Target};
use thunderstore_engine::client::ThunderstoreClient;
use thunderstore_engine::ecosystem::Ecosystem;
use thunderstore_engine::models::PackageIndex;
use thunderstore_engine::profile;

/// Installs each mod and its full dependency closure into the target.
///
/// The engine resolves, downloads, extracts into the shared package cache under
/// `target.base`, installs into `target.dir`, and upserts `mods.yml` plus any
/// `_state` tracker. Nothing here duplicates that pipeline.
///
/// `mods` is passed as both the desired set and the explicit set, so a mod
/// named on the command line is left enabled: asking for it by name is a
/// request to have it active. Only an incidentally reinstalled dependency keeps
/// its disabled state, which `install_batch` restores after the whole batch,
/// even when the batch failed partway.
///
/// Kept as the Thunderstore-only entry point [`run_with_sources`] is layered
/// over, so the bulk of this module's tests can exercise the shared pipeline
/// without needing a multi-source fixture. `main` always calls
/// `run_with_sources`.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn run(
  client: &ThunderstoreClient,
  eco: &Ecosystem,
  target: &Target,
  mods: &[String],
) -> AppResult<()> {
  let index = client.get_manifest().await?;

  install_with_index(client, &index, eco, target, mods).await?;

  Ok(())
}

/// [`run`], resolving against every configured source's merged manifest rather
/// than Thunderstore alone.
///
/// Downloads go through `sources[0]`'s client: any one source's client can
/// download a file resolved from any other source, since the engine's download
/// layer is not scoped to a client's own `base_url`.
///
/// Records which source each succeeded mod came from in the target's
/// `.vmm_state.json` sidecar.
///
/// Installs the named mods at their latest version. Mods pinned by an earlier
/// list import keep their pinned version unless `track_latest` is set.
pub async fn run_with_sources(
  sources: &[Box<dyn ModSource>],
  eco: &Ecosystem,
  target: &Target,
  mods: &[String],
  track_latest: bool,
) -> AppResult<()> {
  // The named mods go to latest, so their pins are dropped; pins on anything
  // else (their already-pinned dependencies included) still apply.
  let pins = match track_latest {
    true => std::collections::HashMap::new(),
    false => {
      crate::sources::remove_pins(target, mods)?;

      crate::sources::read_pins(target)
    }
  };

  let (index, source_map) =
    crate::sources::merged_manifest_pinned(&crate::sources::as_refs(sources), false, &pins).await?;
  let download_client = crate::sources::first_download_client(sources)?;

  let outcome = install_with_index(download_client, &index, eco, target, mods).await?;

  crate::sources::record_sources(
    target,
    outcome
      .iter()
      .filter_map(|name| source_map.get(name).map(|id| (name.clone(), *id))),
  )
}

/// The shared install pipeline: plans the batch, installs it against an
/// already-resolved `index`, reports what happened, and returns every
/// identifier that succeeded (the named mods plus their dependency closure).
async fn install_with_index(
  download_client: &ThunderstoreClient,
  index: &PackageIndex,
  eco: &Ecosystem,
  target: &Target,
  mods: &[String],
) -> AppResult<Vec<String>> {
  // Read the record first, not for its own sake: the planner below reads the
  // same file immediately after, but reading it here first means an unreadable
  // mods.yml speaks in vmm's advice voice instead of the engine's raw error
  // surfacing from inside the planner.
  super::read_modlist(target)?;

  let batch = profile::plan_install_batch(&target.dir, mods, mods)?;

  let outcome = profile::install_batch(
    &target.dir,
    &target.base,
    eco,
    index,
    download_client,
    GAME,
    &batch,
    thunderstore_engine::profile::modlist::now_millis(),
  )
  .await?;

  super::report_installed(target, &outcome.succeeded)?;

  for full_name in &batch.protect_disabled {
    if outcome.succeeded.contains(full_name) {
      println!("kept {full_name} disabled");
    }
  }

  super::report_batch_failures(&outcome)?;

  Ok(outcome.succeeded)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::test_support::Fixture;
  use thunderstore_engine::ecosystem::Ecosystem;
  use thunderstore_engine::profile::modlist;
  use tokio::runtime::Runtime;

  #[test]
  fn install_places_files_and_records_mods_yml() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    assert!(
      target
        .dir
        .join("BepInEx/plugins/Owner-ModA/ModA.dll")
        .exists()
    );

    let mods = modlist::read(&target.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Owner-ModA");
    assert!(mods[0].enabled);
  }

  #[test]
  fn install_extracts_into_the_base_cache_not_the_target() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    // The package cache is shared across targets and must never land inside the
    // game directory.
    let cached = thunderstore_engine::profile::cache::package_cache_dir(
      &target.base,
      crate::target::GAME,
      "Owner-ModA",
      "1.0.0",
    );

    assert!(cached.join("plugins/ModA.dll").exists());
  }

  #[test]
  fn install_pulls_in_the_dependency_closure() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModC".to_string()],
      ))
      .unwrap();

    // Owner-ModC depends on Owner-ModA-1.0.0, so both install.
    let mods = modlist::read(&target.dir).unwrap();

    assert_eq!(mods.len(), 2);
    assert!(modlist::find(&mods, "Owner-ModA").is_some());
    assert!(modlist::find(&mods, "Owner-ModC").is_some());
    assert!(
      target
        .dir
        .join("BepInEx/plugins/Owner-ModA/ModA.dll")
        .exists()
    );
    assert!(
      target
        .dir
        .join("BepInEx/plugins/Owner-ModC/ModC.dll")
        .exists()
    );
  }

  #[test]
  fn installing_a_disabled_mod_by_name_re_enables_it() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    thunderstore_engine::profile::set_enabled_in(
      &target.dir,
      &eco,
      crate::target::GAME,
      "Owner-ModA",
      false,
    )
    .unwrap();

    // Naming a mod explicitly is a request to have it active, so the reapply
    // pass must skip it.
    runtime
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    let mods = modlist::read(&target.dir).unwrap();

    assert!(modlist::find(&mods, "Owner-ModA").unwrap().enabled);
    assert!(
      target
        .dir
        .join("BepInEx/plugins/Owner-ModA/ModA.dll")
        .exists()
    );
  }

  #[test]
  fn a_disabled_mod_reinstalled_as_a_dependency_stays_disabled() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();
    let runtime = Runtime::new().unwrap();

    runtime
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    thunderstore_engine::profile::set_enabled_in(
      &target.dir,
      &eco,
      crate::target::GAME,
      "Owner-ModA",
      false,
    )
    .unwrap();

    // The other half of `installing_a_disabled_mod_by_name_re_enables_it`.
    // Owner-ModC depends on Owner-ModA, so this reinstalls ModA without anyone
    // naming it, and nothing about installing ModC is a request to turn ModA
    // back on. The batch protects that state, and `run` reports each mod it kept
    // disabled so a silently inert dependency is not left to be discovered in
    // game.
    runtime
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModC".to_string()],
      ))
      .unwrap();

    let mods = modlist::read(&target.dir).unwrap();

    assert!(!modlist::find(&mods, "Owner-ModA").unwrap().enabled);
    assert!(modlist::find(&mods, "Owner-ModC").unwrap().enabled);
  }

  #[test]
  fn install_into_a_profile_never_writes_to_the_game_dir() {
    let fixture = Fixture::new();
    let target = fixture.profile_target("experiment");
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap();

    assert!(
      target
        .dir
        .join("BepInEx/plugins/Owner-ModA/ModA.dll")
        .exists()
    );
    assert_eq!(modlist::read(&target.dir).unwrap().len(), 1);
    // The point of profile mode: the game directory is untouched.
    assert!(
      !fixture
        .game_dir
        .path()
        .join("BepInEx/plugins/Owner-ModA")
        .exists()
    );
    assert!(!fixture.game_dir.path().join("mods.yml").exists());
  }

  #[test]
  fn the_fixture_can_install_a_recognised_mod_loader() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["denikson-BepInExPack_Valheim".to_string()],
      ))
      .unwrap();

    // The ecosystem must recognise it as a loader, or every loader-specific
    // code path stays untested.
    assert!(
      eco
        .modloader_package("denikson-BepInExPack_Valheim")
        .is_some()
    );
    // A loader pack unpacks to the target root with its `rootFolder` stripped,
    // and is recorded as `State` so it has an install record.
    assert!(target.dir.join("winhttp.dll").exists());
    assert!(target.dir.join("BepInEx/core/BepInEx.dll").exists());
    assert!(
      thunderstore_engine::install::state_file_path(&target.dir, "denikson-BepInExPack_Valheim")
        .exists()
    );
  }

  #[test]
  fn an_unreadable_mods_yml_reports_its_path_before_installing() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    std::fs::write(target.mods_yml(), "not: [valid").unwrap();

    let message = Runtime::new()
      .unwrap()
      .block_on(run(
        &fixture.client,
        &eco,
        &target,
        &["Owner-ModA".to_string()],
      ))
      .unwrap_err()
      .to_string();

    // vmm's own advice voice, the same shape `list`/`update`/`uninstall` use,
    // not the engine's raw parse error leaking through the planner.
    assert!(message.contains("the install record for"), "got: {message}");
    assert!(message.contains(&target.mods_yml().display().to_string()));
  }

  #[test]
  fn install_reports_an_unknown_mod() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    let result = Runtime::new().unwrap().block_on(run(
      &fixture.client,
      &eco,
      &target,
      &["Owner-Nonexistent".to_string()],
    ));

    assert!(result.is_err());
  }

  #[test]
  fn run_with_sources_installs_a_mod_that_only_exists_on_the_second_source() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(run_with_sources(
        &fixture.multi_sources(),
        &eco,
        &target,
        &["Hexium-OnlyMod".to_string()],
        false,
      ))
      .unwrap();

    let mods = modlist::read(&target.dir).unwrap();

    assert_eq!(mods.len(), 1);
    assert_eq!(mods[0].name, "Hexium-OnlyMod");

    // The sidecar records which source it came from.
    let recorded = crate::sources::read_sources(&target);

    assert_eq!(
      recorded.get("Hexium-OnlyMod"),
      Some(&crate::sources::SourceId::Hexium)
    );
  }

  #[test]
  fn run_with_sources_records_thunderstore_for_a_thunderstore_only_mod() {
    let fixture = Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    Runtime::new()
      .unwrap()
      .block_on(run_with_sources(
        &fixture.multi_sources(),
        &eco,
        &target,
        &["Owner-ModA".to_string()],
        false,
      ))
      .unwrap();

    let recorded = crate::sources::read_sources(&target);

    assert_eq!(
      recorded.get("Owner-ModA"),
      Some(&crate::sources::SourceId::Thunderstore)
    );
  }
}
