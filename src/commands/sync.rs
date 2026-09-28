use crate::config::GaleSyncConfig;
use crate::error::AppResult;
use crate::gale_sync;
use crate::sources::ModSource;
use crate::target::Target;
use thunderstore_engine::ecosystem::Ecosystem;

/// Fetches the configured gale-sync profile and reconciles the target against
/// it: installs what it names and uninstalls what it no longer does.
///
/// The fetch happens before anything on disk is touched, so an unreachable
/// gale-sync (or an unconfigured one) fails here with the target left exactly
/// as it was. Callers that want "keep running with whatever is installed" on
/// failure, like `scripts/vmm-update-and-restart.sh`, get that by tolerating
/// this command's non-zero exit rather than vmm guessing a policy.
pub async fn run(
  config: &GaleSyncConfig,
  eco: &Ecosystem,
  target: &Target,
  sources: &[Box<dyn ModSource>],
) -> AppResult<()> {
  let desired = gale_sync::fetch_current(config).await?;

  println!(
    "syncing to gale-sync profile \"{}\" ({} mods)",
    desired.profile_name,
    desired.mods.len()
  );

  super::portability::reconcile_zip(eco, target, sources, &desired.zip_bytes).await
}
