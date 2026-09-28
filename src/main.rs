mod cli;
mod commands;
mod config;
mod error;
mod gale_sync;
mod logs;
mod progress;
mod sources;
mod target;
#[cfg(test)]
mod test_support;

use crate::{
  cli::AppCli,
  error::{AppError, AppResult},
  progress::IndicatifProgress,
  target::Target,
};
use clap::Parser;
use cli::{Command, UpdatesCommand};
use config::{GameDirStatus, get_config};
use std::cell::LazyCell;
use std::sync::Arc;
use thunderstore_engine::client::ThunderstoreClient;
use thunderstore_engine::ecosystem::Ecosystem;

/// Thunderstore's canonical host.
///
/// This must be the bare host, not a per-community subdomain: the client
/// composes the index URL as `{BASE_URL}/c/{COMMUNITY}/api/v1/package/`, but the
/// profile share endpoints (`/api/experimental/legacyprofile/...`) carry no
/// community segment at all. A subdomain like `valheim.thunderstore.io` supplies
/// its own community on redirect, which would silently point the profile
/// endpoints at the wrong community, and the 302 would drop the POST body
/// besides.
const BASE_URL: &str = "https://thunderstore.io";
/// The Valheim community slug on Thunderstore.
const COMMUNITY: &str = "valheim";

/// Resolves the game directory for installs, or prints actionable guidance and
/// exits when it cannot proceed.
///
/// The message is written to stderr unconditionally (not via the log filter) so
/// it is always seen, including by users on the default `log_level = "error"`.
fn require_game_dir(config: &config::AppConfig) -> &str {
  match config.game_dir_status() {
    GameDirStatus::Set(dir) => dir,
    GameDirStatus::Unset => report_and_exit(unset_game_dir_error()),
  }
}

/// The refusal [`require_game_dir`] exits with when `game_dir` is not set.
/// Pulled out as a pure function so the wording is testable, since
/// [`require_game_dir`] ends in [`report_and_exit`], which never returns.
fn unset_game_dir_error() -> AppError {
  AppError::advice(
    "`game_dir` is not set, so there is nowhere to install mods.",
    format!(
      "Set `game_dir` to your Valheim game folder, for example:\n\n\
       \x20   game_dir = \"{example}\"\n\n\
       Nothing was changed.",
      example = config::example_game_dir(),
    ),
    &[],
  )
}

/// The refusal `run` returns when `game_dir` resolves to something that is not a
/// directory.
///
/// Worth a check of its own because nothing downstream performs one: both
/// `apply_install` and the record writer `create_dir_all` their way to the
/// route they were handed, so a typo'd `game_dir`, or one whose `~`/`$HOME`
/// never expanded, quietly grows a fresh `BepInEx/plugins` tree at that path
/// (relative to the working directory, in the unexpanded case) and every
/// command reports success while the real game stays untouched.
///
/// Pulled out as a pure function so its wording is testable: `run` itself parses
/// argv and cannot be called from a test.
fn missing_game_dir_error(dir: &std::path::Path) -> AppError {
  AppError::advice(
    "the configured `game_dir` is not a directory.",
    format!(
      "vmm looked for it at:\n\n\x20   {}\n\nNothing was changed. Check the path \
       in your config against where the game is actually installed. `~` and \
       environment variables like `$HOME` are both expanded, so either form \
       works, but a name that is not set is left as written and would show up \
       above exactly as you typed it.",
      dir.display()
    ),
    &[],
  )
}

/// Routes a toggle to the single-mod or whole-target path.
///
/// `clap` guarantees exactly one of `name` and `all` is set
/// (`required_unless_present` plus `conflicts_with`), so the `None` case is
/// unreachable rather than a silent no-op.
fn toggle_dispatch(
  ecosystem: &Ecosystem,
  target: &Target,
  args: &cli::ToggleArgs,
  enabled: bool,
) -> AppResult<()> {
  match &args.name {
    Some(name) => commands::toggle::run(ecosystem, target, name, enabled),
    None => commands::toggle::run_all(ecosystem, target, enabled),
  }
}

/// Prints `error` in the house style and exits non-zero.
///
/// The `vmm: ` prefix lives here and nowhere else, so no message body carries
/// its own. `Display` is used deliberately: returning `AppResult` from `main`
/// instead would let Rust's `Termination` impl format with `Debug`, which is
/// what produced output like `Error: Other("...")`.
#[cfg(not(tarpaulin_include))]
fn report_and_exit(error: AppError) -> ! {
  eprintln!("vmm: {error}");

  std::process::exit(1)
}

#[tokio::main]
#[cfg(not(tarpaulin_include))]
async fn main() {
  if let Err(error) = run().await {
    report_and_exit(error);
  }
}

/// The real entry point. Separate from `main` so every failure path returns an
/// error to one printer rather than formatting its own.
#[cfg(not(tarpaulin_include))]
async fn run() -> AppResult<()> {
  let app = AppCli::parse();
  let config = get_config(app.config.as_deref())
    .unwrap_or_else(|err| panic!("An error has occurred getting the config: '{err}'"));

  logs::setup_logging(&config.log_level);
  tracing::info!("Starting valheim mod manager");

  let base = config.base_dir();
  let progress: Arc<dyn thunderstore_engine::progress::ProgressReporter> =
    Arc::new(IndicatifProgress::new());

  let client = ThunderstoreClient::builder()
    .base_url(BASE_URL)
    .community(COMMUNITY)
    .cache_dir(&base)
    .progress(progress.clone())
    .build()?;

  let mod_sources = sources::build_sources(&config.enabled_sources(), &base, progress.clone())?;

  // Commands that never touch an install target dispatch before one is
  // resolved, so they work with no `game_dir` configured.
  match &app.command {
    Command::Search(args) => {
      return commands::search::run_multi(&mod_sources, args.source, &args.term).await;
    }
    Command::Update(sub) if matches!(sub.command, UpdatesCommand::Manifest) => {
      return commands::update::run_manifest_multi(&mod_sources).await;
    }
    _ => {}
  }

  let game_dir = config::expand_path(require_game_dir(&config));

  if !game_dir.is_dir() {
    return Err(missing_game_dir_error(&game_dir));
  }

  let target = target::resolve(base, game_dir)?;

  // The bundled snapshot is zstd-decompressed and parsed on construction, so
  // build it only if a command actually consults the install rules.
  let ecosystem = LazyCell::new(Ecosystem::bundled);

  match &app.command {
    // Matched on the subcommand rather than assumed: `Manifest` returned above,
    // but a third `UpdatesCommand` variant must not silently reinstall every
    // recorded mod, so the compiler is made to demand an arm for it.
    Command::Update(sub) => match sub.command {
      UpdatesCommand::Mods => {
        commands::update::run_mods_with_sources(&mod_sources, &ecosystem, &target).await?
      }
      UpdatesCommand::Manifest => unreachable!("dispatched before target resolution"),
    },
    Command::List(list_args) => commands::list::run(&target, &list_args.format)?,
    Command::Install(args) => {
      commands::install::run_with_sources(&mod_sources, &ecosystem, &target, &args.mods).await?
    }
    Command::Uninstall(args) => match args.all {
      true => commands::uninstall::run_all(
        &ecosystem,
        &target,
        args.force,
        args.yes,
        commands::uninstall::confirm_uninstall_all,
      )?,
      false => commands::uninstall::run(&ecosystem, &target, &args.mods, args.force)?,
    },
    Command::Enable(args) => toggle_dispatch(&ecosystem, &target, args, true)?,
    Command::Disable(args) => toggle_dispatch(&ecosystem, &target, args, false)?,
    Command::Export(args) => match args.code {
      true => commands::portability::export_code(&client, &target).await?,
      false => commands::portability::export_file(&target)?,
    },
    Command::Import(args) => {
      commands::portability::import(
        &client,
        &ecosystem,
        &target,
        &args.source,
        !args.additive,
        &mod_sources,
      )
      .await?
    }
    Command::Sync => {
      commands::sync::run(&config.gale_sync, &ecosystem, &target, &mod_sources).await?
    }
    Command::Search(_) => {
      unreachable!("dispatched before target resolution")
    }
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::path::Path;

  #[test]
  fn a_game_dir_that_is_not_a_directory_names_the_path_that_was_looked_for() {
    let message = missing_game_dir_error(Path::new("/games/Valhiem")).to_string();

    // The path as vmm resolved it, so a typo or an unexpanded variable is
    // visible in the message rather than inferred from a later surprise.
    assert!(
      message.contains("/games/Valhiem"),
      "the error must name the path it looked for; got: {message}"
    );
    assert!(
      message.contains("game_dir"),
      "the error must name the setting to fix; got: {message}"
    );
    assert!(message.contains("Nothing was changed"), "got: {message}");
  }

  #[test]
  fn the_unset_refusal_names_the_setting_and_shows_an_example() {
    let message = unset_game_dir_error().to_string();

    assert!(message.contains("game_dir"), "got: {message}");
    assert!(
      message.contains(config::example_game_dir()),
      "the refusal must show a usable example; got: {message}"
    );
    assert!(message.contains("Nothing was changed"), "got: {message}");
  }

  #[test]
  fn toggle_dispatch_routes_by_whether_a_mod_was_named() {
    let fixture = test_support::Fixture::new();
    let target = fixture.target();
    let eco = Ecosystem::bundled();

    let named = cli::ToggleArgs {
      name: Some("Owner-ModA".to_string()),
      all: false,
    };
    let every = cli::ToggleArgs {
      name: None,
      all: true,
    };

    // `clap` guarantees exactly one of the two is set, so the routing is the
    // only thing standing between `--all` and a silent no-op. Owner-ModA is not
    // installed here, so the single-mod route refuses while the whole-target one
    // has nothing to do and succeeds: two different outcomes, which is what
    // proves they went to different places.
    assert!(toggle_dispatch(&eco, &target, &named, false).is_err());
    assert!(toggle_dispatch(&eco, &target, &every, false).is_ok());
  }
}
