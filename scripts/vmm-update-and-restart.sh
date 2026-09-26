#!/bin/sh
# Intended for POST_UPDATE_CHECK_HOOK on
# ghcr.io/community-valheim-tools/valheim-server. Runs vmm's own update
# commands, then restarts the running server only if something actually
# changed: neither this image nor vmm itself does that on its own, since
# vmm's mods.yml has no notion of a running server process to signal.
#
# thunderstore-engine (vmm's backing crate) always installs into
# "$VMM_GAME_DIR/BepInEx/{plugins,config,patchers}" - a shape this image
# doesn't use anywhere on its own. Rather than write there directly (which
# would mean duplicating the image's own config->live sync, or getting wiped
# by it), this script symlinks those subfolders into the image's real,
# persistent BEPINEX_CONFIG_DIR, the same directory the image's docs already
# tell you to drop plugins into by hand and that its own bepinex-updater
# already knows how to sync into the live install on every boot and every
# update. See the README's "Adding vmm to the official community Docker
# image" section for the full rationale.
#
# Config:
#   VMM_CONFIG         path to vmm's config file (default: /config/vmm_config.toml)
#   VMM_BIN            the vmm binary (default: vmm, resolved via PATH)
#   VMM_GAME_DIR       must match `game_dir` in VMM_CONFIG (default: /config/vmm_game)
#   BEPINEX_CONFIG_DIR the image's persistent BepInEx directory (default: /config/bepinex)
set -eu

VMM_CONFIG="${VMM_CONFIG:-/config/vmm_config.toml}"
VMM_BIN="${VMM_BIN:-vmm}"
VMM_GAME_DIR="${VMM_GAME_DIR:-/config/vmm_game}"
BEPINEX_CONFIG_DIR="${BEPINEX_CONFIG_DIR:-/config/bepinex}"

if ! command -v "$VMM_BIN" >/dev/null 2>&1; then
  echo "vmm-update-and-restart: '$VMM_BIN' not found on PATH; is POST_BOOTSTRAP_HOOK installing it?" >&2
  exit 1
fi

# Idempotent - safe on every tick. ln -sfn replaces a stale/wrong symlink
# left over from an older BEPINEX_CONFIG_DIR/VMM_GAME_DIR without complaint.
mkdir -p "$BEPINEX_CONFIG_DIR/plugins" "$BEPINEX_CONFIG_DIR/patchers" "$VMM_GAME_DIR/BepInEx"
ln -sfn "$BEPINEX_CONFIG_DIR/plugins" "$VMM_GAME_DIR/BepInEx/plugins"
ln -sfn "$BEPINEX_CONFIG_DIR/patchers" "$VMM_GAME_DIR/BepInEx/patchers"
ln -sfn "$BEPINEX_CONFIG_DIR" "$VMM_GAME_DIR/BepInEx/config"

# vmm's own tracing output is written to stdout alongside command output (see
# src/logs.rs), and it's timestamped, so a raw capture of `list` would differ
# between the "before" and "after" snapshots below even when nothing
# installed actually changed. The JSON payload is the only thing that starts
# a line with `[`, so trimming everything before that reliably isolates it
# regardless of the configured log_level.
list_json() {
  "$VMM_BIN" --config "$VMM_CONFIG" list --format json | sed -n '/^\[/,$p'
}

"$VMM_BIN" --config "$VMM_CONFIG" update manifest

before=$(list_json)

"$VMM_BIN" --config "$VMM_CONFIG" update mods

after=$(list_json)

if [ "$before" = "$after" ]; then
  echo "vmm-update-and-restart: no mod changes, leaving the server running"
  exit 0
fi

echo "vmm-update-and-restart: mods changed, restarting valheim-server"
supervisorctl restart valheim-server
