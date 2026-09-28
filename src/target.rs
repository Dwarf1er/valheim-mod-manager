use crate::error::AppResult;
use std::path::PathBuf;

/// The Valheim community slug, which is also the engine's ecosystem game key.
pub const GAME: &str = "valheim";

/// Where this invocation installs and tracks from. The engine owns the
/// layout; vmm adds only the wording below.
pub type Target = thunderstore_engine::profile::target::InstallTarget;

/// A phrase naming `target` for use inside a sentence, so a message can say which
/// install root it is about without the caller branching on the mode.
///
/// A free function rather than a method because `Target` is a foreign type. This
/// is the only part of the target concept that stays in vmm: it is wording, and a
/// desktop client would write its own.
pub fn describe(target: &Target) -> String {
  match &target.profile {
    Some(name) => format!("profile {name:?}"),
    None => "the game directory".to_string(),
  }
}

/// Resolves the install target: always the game directory.
pub fn resolve(base: PathBuf, game_dir: PathBuf) -> AppResult<Target> {
  Ok(Target::resolve(base, game_dir, GAME, None)?)
}

#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  #[test]
  fn describe_names_the_profile_or_the_game_directory() {
    let base = tempdir().unwrap();
    let game_dir = tempdir().unwrap();

    let game = Target::resolve(
      base.path().to_path_buf(),
      game_dir.path().to_path_buf(),
      GAME,
      None,
    )
    .unwrap();

    assert_eq!(describe(&game), "the game directory");

    thunderstore_engine::profile::layout::create(base.path(), GAME, "experiment").unwrap();

    let profile = Target::resolve(
      base.path().to_path_buf(),
      game_dir.path().to_path_buf(),
      GAME,
      Some("experiment"),
    )
    .unwrap();

    assert_eq!(describe(&profile), "profile \"experiment\"");
  }
}
