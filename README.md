# Valheim Mod Manager (vmm)

[![Build Status](https://github.com/endoze/valheim-mod-manager/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/endoze/valheim-mod-manager/actions?query=branch%3Amaster)
[![Coverage Status](https://coveralls.io/repos/github/endoze/valheim-mod-manager/badge.svg?branch=master)](https://coveralls.io/github/endoze/valheim-mod-manager?branch=master)
[![Crate](https://img.shields.io/crates/v/valheim-mod-manager.svg)](https://crates.io/crates/valheim-mod-manager)

A command-line tool for managing and automatically downloading Valheim mods and their dependencies.

## Features

- Installs mods and their full dependency closure from Thunderstore or Hexium
- Tracks what is installed in an r2modman-compatible `mods.yml`, so an install
  can be uninstalled exactly, without guessing from directory contents
- Enables and disables mods in place, without uninstalling them
- Installs into a single target: your `game_dir`
- Exports and imports profiles as `.r2z` files, Thunderstore/Gale profile codes, or
  live r2modman/Gale profile directories, where file/code imports reconcile by default (`--additive` to opt out),
  plus `vmm sync` against a gale-sync profile
- Shares one download and extraction cache across every target
- Shows live progress while it works: a spinner for the package index fetch and
  a byte bar per mod download

## Installation

### From Source

```bash
# Clone the repository
git clone https://github.com/Endoze/valheim-mod-manager.git
cd valheim-mod-manager

# Install the application
cargo install --path .

# The binary will be installed in your Cargo bin directory
```

## Configuration

`vmm` looks for configuration in the following order:

1. A `vmm_config.toml` in the current directory (local config)
2. `~/.config/vmm/vmm_config.toml` (global XDG config)

If neither exists, a default config is created at the global location on first run.

You can also specify a config file directly with the `--config` flag, which bypasses the above lookup entirely.

The config file supports the following settings:

- `log_level`: Logging verbosity (`error`, `warn`, `info`, `debug`, `trace`)
- `game_dir`: Your Valheim game folder, the directory holding the game
  executable, where the mod loader is installed
- `track_latest`: Optional, default `false`. By default the versions named by
  an imported list (gale-sync, `.r2z`, profile code, r2modman directory) are
  respected: each mod is installed at the listed version and `vmm update mods`
  keeps it there. Set `track_latest = true` to ignore listed versions and keep
  every mod on its latest. See [Version pinning](#version-pinning).
- `data_dir`: Optional. Where the package cache and exports live.
  Defaults to `~/.config/vmm`
- `[sources] enabled`: Which mod sources to resolve and install from, in
  priority order. Defaults to `["thunderstore"]`; add `"hexium"` to also pull
  from [Hexium](https://valheim.hexium.gg):
  ```toml
  [sources]
  enabled = ["thunderstore", "hexium"]
  ```
  `vmm search --source hexium` restricts a search to one source for that
  invocation. See
  [How Source Resolution Works](#how-source-resolution-works) for exactly how sources are merged, which
  source a shared mod resolves from, and which client downloads it.
- `[gale_sync] profile_id`: Optional. The id of a
  [gale-sync](https://github.com/Kesomannen/gale-sync) profile whose modlist is
  the desired one. Reading a profile needs no token, so none is stored.
  Unset disables gale-sync. `base_url` overrides the API root
  (default `https://gale.kesomannen.com/api`).
  ```toml
  [gale_sync]
  profile_id = "GsioqKpVRwiP7_ynX-QsuA"
  ```
  Run `vmm sync` to reconcile against it (see
  [Syncing with gale-sync](#syncing-with-gale-sync)).

`game_dir` and `data_dir` both expand a leading `~` and environment variables,
so `~/valheim` and `$HOME/valheim` are equivalent. A variable that is not
set is left as written. `vmm` refuses to run rather than installing into a
`game_dir` that does not exist, so a typo is reported instead of silently
creating a fresh mod tree somewhere else.

Example configuration:

```toml
log_level = "info"
game_dir = "/config/vmm_game"
```

## Usage

### Managing mods

```bash
# Install mods and their dependencies
vmm install denikson-BepInExPack_Valheim ValheimModding-Jotunn

# List what is installed
vmm list
vmm list --format json

# Take a mod out of play without uninstalling it
vmm disable ValheimModding-Jotunn
vmm enable ValheimModding-Jotunn

# Disable or enable every installed mod at once (the mod loader is left alone)
vmm disable --all
vmm enable --all

# Remove a mod, its files, and its tracked state
vmm uninstall ValheimModding-Jotunn

# Remove it even though unrecorded mod folders are present (deletes those too)
vmm uninstall --force ValheimModding-Jotunn

# Remove every installed mod, including the mod loader, leaving a vanilla install
vmm uninstall --all
vmm uninstall --all --yes

# Update every installed mod (pinned mods stay at their pinned version;
# see Version pinning)
vmm update mods

# Refresh the cached package index (every configured source)
vmm update manifest

# Search every configured source
vmm search jotunn

# Restrict a search to one source
vmm search --source hexium jotunn
```

`vmm enable --all` and `vmm disable --all` apply to every mod recorded in the
current target's `mods.yml`; the mod loader is left out of the batch, since it
has no disabled state. A mod already in the requested state is counted
separately as unchanged rather than as a failure. If one mod's toggle fails,
the rest of the batch still applies and `vmm` exits non-zero, naming what
failed.

`vmm update mods` reinstalls every recorded mod on every run, not only when a
mod's version changes. That overwrites any file a mod packages inside its own
folder under `BepInEx/plugins/`, including hand edits you've made to those
files. Files you place directly under `BepInEx/config/` are left alone if they
already exist. Mods you have disabled stay disabled.

#### `mods.yml` owns the whole mod tree

`mods.yml` is authoritative over every per-mod folder under the install routes
(`BepInEx/plugins/<Owner-ModName>/` and the other namespaced routes), not just
over the mods `vmm` happens to have installed. An uninstall reconciles that whole
tree against `mods.yml`, so **any `<Owner-ModName>` folder it does not record can
be removed by an uninstall**, including mods you installed by hand, mods
r2modman put there. In `game_dir` mode that tree is your live game directory.

`vmm uninstall` therefore checks first: if it finds mod folders `mods.yml` does
not record, it names them and refuses without changing anything. Bring them under
management (`vmm install <Owner-ModName>`), or re-run with `--force` to remove the named mods and accept that
those folders go too. Folders that are not `<Owner-ModName>`-shaped, such as
loader directories like `BepInEx/plugins/MMHOOK` and anything directly under
`BepInEx/config/`, are never touched.

`vmm uninstall --all` removes every mod recorded for the current target,
including the mod loader, leaving a vanilla install; files under
`BepInEx/config` are left alone regardless. It asks for confirmation before
removing anything, listing what will go, and `--yes` skips that prompt. With no
terminal available and no `--yes`, it refuses rather than assuming an answer.

Untracked mod folders are handled a little differently here than for a
single-mod uninstall: the confirmation prompt lists them too, so accepting it
covers them without needing `--force`. `--yes` skips that same prompt, though,
so on that path `--force` is still required to accept those folders being
removed alongside everything else.

### Sharing

```bash
# Write an .r2z into <data_dir>/valheim/exports/
vmm export

# Upload and print a shareable Thunderstore profile code
vmm export --code

# Import from a file, a profile code, an r2modman profile directory, or a
# live Gale profile directory
vmm import ./default_1753488000.r2z
vmm import a1b2c3d4-0000-0000-0000-000000000000
vmm import ~/.config/r2modmanPlus-local/Valheim/profiles/Default
vmm import ~/.local/share/gale/valheim/profiles/Default

# Only add: keep mods the re-imported source no longer names, instead of
# uninstalling them (file or code sources only, see below)
vmm import --additive ./default_1753488000.r2z
```

A file or profile-code import installs each mod at the version the list names
(see [Version pinning](#version-pinning)) and prints what it installed. Every mod it names is resolved through the same multi-source merge
`install` uses (see [How It Works](#how-it-works)), so a mod that only exists
on a non-default configured source (e.g. Hexium) is still found, even though
neither `.r2z` files nor profile codes have any field to record which source
a mod was originally installed from.

A profile code is fetched from Thunderstore's `legacyprofile` endpoint by
default. Gale uploads a profile to Hexium's identical endpoint instead when
it contains a Hexium-exclusive mod, and warns that the code only works in
Gale. That warning does not apply to vmm: when `hexium` is a configured
source, `vmm import` checks Hexium's endpoint first and falls back to
Thunderstore if the code is not found there.

Importing an r2modman profile directory (one with a `mods.yml`) adopts it
as-is at the versions the source recorded, and downloads nothing, with one
exception: if the adopted `mods.yml` names a mod loader, the loader is
reinstalled afterward (at the version the `mods.yml` recorded, resolved
through that same multi-source merge) so it gains the install record that
makes it manageable and removable; a source naming no loader stays fully
offline. It is also a raw copy with no pre-clean: if the destination already
has different mods installed, `mods.yml` is overwritten while the previous
mods' files stay on disk, now untracked and orphaned. Import into an empty
target, or accept that leftover.

**Gale** keeps its live profile state in SQLite rather than a per-profile
`mods.yml`, so a live Gale profile directory has mod files on disk but no
record. Importing one is recognized automatically (a directory with no
`mods.yml`) and, unlike an r2modman directory, reinstalls **every** mod it
finds from the configured source(s) rather than merely copying files, since
nothing can attribute a copied file to a specific mod by shape. Every mod
therefore lands at its latest version, since a Gale directory records no
versions to pin, and each is recorded in the `.vmm_state.json` sidecar with whichever
source actually supplied it. A Gale-exported `.r2z` file or shared profile
code, by contrast, already works today unchanged through the ordinary file/code
import path above; only a *live* Gale profile directory needs this route.

### Reconciling a shrinking export

A file or profile-code import is authoritative by default: after installing,
it also uninstalls anything the target has that the source no longer names, so
re-importing a shared profile makes the target converge on it rather than only
ever growing. Pass `--additive` to keep the old mods instead (for example to
try a profile locally without disturbing what is already installed):

```bash
vmm import a1b2c3d4-0000-0000-0000-000000000000
vmm import --additive a1b2c3d4-0000-0000-0000-000000000000
```

Directory imports (Gale or r2modman alike) never reconcile: they are a raw
copy of whatever is on disk, so the default does not apply to them. A stale mod
that cannot be removed exactly (for example, a mod loader adopted from a
directory with no install record) is reported and left in place rather than
failing the whole run; anything else stale is still removed.

### Version pinning

Lists carry a version for every mod (an `.r2z` or profile code's `export.r2x`,
a gale-sync profile, or an r2modman directory's `mods.yml`; dependencies are
listed like any other mod). By default vmm respects them:

- `import` and `sync` install each mod at its listed version and remember it in
  `.vmm_state.json` next to `mods.yml`.
- `update mods` keeps pinned mods at their pinned version and moves everything
  else to latest.
- `vmm install X` by hand installs X at its latest version and drops X's pin.
  X's dependencies that are already pinned stay pinned; new ones come in at
  latest.
- If a listed version is no longer offered by any configured source, that mod
  is skipped with a warning and whatever is installed stays as it is.
- A live Gale profile directory records no versions, so it installs latest and
  pins nothing.
- When the same package is on several sources, a source that has the pinned
  version wins over one that is merely newer.

Set `track_latest = true` in the config to ignore listed versions entirely:
nothing is pinned and everything goes to latest, as before.

### Syncing with gale-sync

With `[gale_sync] profile_id` set, `vmm sync` fetches that profile and
reconciles the target against it exactly like a pruning import. If gale-sync
cannot be reached (or no profile is configured) it fails before touching
anything. `scripts/vmm-update-and-restart.sh` runs it on every cycle and
tolerates that failure, keeping the installed mods, then restarts the server
if the mod list changed.

## Global Options

### `--config <path>`

Override the config file location, bypassing the local/global lookup:

```bash
vmm --config /path/to/my/vmm_config.toml update mods
```

Downloads and cached data always go to `data_dir` (or `~/.config/vmm` if unset)
regardless of which config file is used. Respects `$XDG_CONFIG_HOME` when
`data_dir` is not set.

## How Source Resolution Works

1. Resolves the target to operate on: `game_dir`
2. Fetches each configured source's package index (`[sources] enabled`,
   `["thunderstore"]` unless you've opted into `"hexium"` too), caching each
   one under `data_dir`, keyed by that source's own URL so multiple sources'
   caches never collide
3. **Merges every source's index into one**, walking sources in the order
   `enabled` lists them:
   - The first time a mod's full name (e.g. `Owner-ModName`) is seen, coming
     from any source, it's added to the merged index tagged with that source.
   - If the same full name turns up again from a later source, its
     `date_updated` is compared against what's already in the merged index: a
     **strictly newer** timestamp replaces the entry (and its source tag);
     an equal or older one is dropped, leaving whichever source already had
     it. A mod that exists on only one source is unaffected either way.
   - Net effect: the most recently updated copy of a shared mod always wins,
     and `enabled`'s order matters only as a **tie-break**; when two sources
     report the exact same `date_updated` for the same mod, whichever is
     listed earlier in `enabled` keeps its copy.
   - `vmm search --source hexium <term>` only filters which catalogs are
     queried and displayed; it does not change how a mod is resolved.
4. Resolves the full dependency closure for the requested mods against that
   one merged index. Dependency resolution itself has no notion of more than
   one source; the merge in step 3 is the only thing multi-source changes
5. Downloads and extracts each resolved package into the shared package
   cache, skipping anything already fetched at the required version. Every
   download goes through the **first-configured source's** HTTP client
   (`enabled[0]`) regardless of which source the package actually resolved
   from: a download is a plain request against the URL in that package's own
   manifest entry, not one scoped to the client's own host, so any configured
   source's client can fetch a file that resolved from any other
6. Installs each package into the target using the mod loader's install
   rules, records it in the target's `mods.yml`, and records which source it
   actually resolved from in `.vmm_state.json`, a sidecar next to
   `mods.yml` (whose r2modman-compatible schema has no field of its own for
   this; it also holds version pins) — written even with only `"thunderstore"` configured, not just once
   a second source is added. `vmm list --format json` reports it per mod, and
   `vmm uninstall` prunes an entry once its mod is gone; a mod with no
   sidecar entry (installed before this sidecar existed) is reported as
   `thunderstore`

Steps 2 onward back every command that resolves mod names by full name:
`install`, `update`, `search`, and all three `import` routes (a file, a
profile code, or a directory). That last one matters because none of those
profile interchange formats carry a per-mod source field, so importing a
profile can never literally restore "the source a mod originally came from,"
there is nothing recorded to restore. What it does instead is resolve every
mod the import names through this same merged-index pipeline, so a mod that
only exists on a non-default source (e.g. Hexium) is found and installed
rather than failing simply because only the default source was ever checked.

## Directory Structure

- `data_dir` (or `~/.config/vmm` if unset, respecting `$XDG_CONFIG_HOME`):
  holds the downloaded package cache, `.r2z` exports,
  under `data_dir/valheim/`
- Your target directory (`game_dir`) holds the installed mod files,
  `mods.yml`, any loader state, and `.vmm_state.json`, recording which
  source each mod came from and which version it is pinned to (see [How It Works](#how-it-works))

## Adding vmm to the official community Docker image

This section covers wiring `vmm` into
[`ghcr.io/community-valheim-tools/valheim-server`](https://github.com/community-valheim-tools/valheim-server-docker),
a community-maintained Valheim dedicated server image, so a running server
container keeps its mods up to date on its own schedule. It needs no custom
image or Dockerfile: the image already exposes documented `*_HOOK` environment
variables that block startup or its update loop until a given shell command
returns, and that hook surface is everything this integration uses.

`thunderstore-engine` (vmm's backing crate) always installs relative to
`game_dir` using a fixed shape: `<game_dir>/BepInEx/{plugins,config,
patchers}`. Neither of the image's own BepInEx paths is that shape.
`/opt/valheim/bepinex` is real-shaped (`BepInEx/plugins` etc. as siblings),
but it is the image's live, ephemeral install. `bepinex-updater` rebuilds it
from scratch (a fresh `rsync` into a `.tmp` directory, then swap) on every
Valheim server update and every BepInEx pack update, so anything `vmm` wrote
directly there would be wiped the next time either happens. `/config/bepinex`
is the image's actual persistent volume and survives that, but it is
flattened: `plugins/` and `patchers/` sit directly under it with no nested
`BepInEx/` segment. The image's own `bepinex-updater` copies its contents
into the live install on every boot and every update
(`sync_bepinex_loadables` in the image's `common` script), and that copy is
what actually keeps mods loaded across updates. This integration uses that
same mechanism instead of duplicating it.

So `game_dir` points at a small directory `vmm` owns exclusively,
`/config/vmm_game`, whose `BepInEx/` subfolders are symlinks into
`/config/bepinex`:

```
/config/vmm_game/BepInEx/plugins  -> /config/bepinex/plugins
/config/vmm_game/BepInEx/patchers -> /config/bepinex/patchers
/config/vmm_game/BepInEx/config   -> /config/bepinex
```

[`scripts/vmm-update-and-restart.sh`](scripts/vmm-update-and-restart.sh)
creates these on every run, idempotently, so there is no separate setup
step. `vmm` then writes through them exactly as it would into an ordinary
game directory, but every file actually lands in `/config/bepinex`, the same
directory the image's own docs tell you to drop plugins into by hand, and
the same directory `bepinex-updater` already knows how to sync into the live
install. Mods placed there by hand, before or alongside `vmm`, are picked up
by the same sync; `vmm` just won't know about them (they carry no `mods.yml`
entry, so `vmm list`/`vmm uninstall` won't see them). A mod's own `.cfg`,
written the first time it runs, lands directly in `/config/bepinex/` (the
`BepInEx/config` symlink points at `/config/bepinex` itself, not a subfolder
of it). That is where to look to hand-edit a mod's settings.

This does not cover every install route `thunderstore-engine` knows about.
`BepInEx/core` (the loader itself) and `BepInEx/monomod` (MonoMod hook DLLs)
are not symlinked, because the image's own sync only ever copies `plugins/`
and `patchers/`. See step 4 of the setup below, "Limits of this approach".

### Setup

1. **`vmm_config.toml`**, committed or mounted at `/config/vmm_config.toml`:

   ```toml
   game_dir = "/config/vmm_game"
   data_dir = "/config/vmm"

   [sources]
   enabled = ["hexium", "thunderstore"]
   ```

   Both live under `/config` so the package cache, `mods.yml`, and (via the
   symlinks described above) the mods themselves all survive container
   recreation. `game_dir` is the shim directory, not either of the image's
   own BepInEx paths (see above). This uses only the current schema,
   `game_dir` and `data_dir`, not the `install_dir`/`cache_dir` keys some other
   integrations write, which would be a schema mismatch against this version
   of `vmm`. List `hexium` first if you use
   it: a Gale-exported profile code for a Hexium-inclusive profile is hosted
   on Hexium's endpoint, and `vmm import` tries sources in this order (see
   [Sharing](#sharing)).

2. **Environment variables**, added to the image's `docker run`/compose.
   This has to happen before step 3: `vmm` is not on the container's `PATH`
   until `POST_BOOTSTRAP_HOOK` puts it there, and that only runs when the
   container is created or restarted.

   ```bash
   -e BEPINEX=true \
   -e POST_BOOTSTRAP_HOOK='curl --proto "=https" --tlsv1.2 -LsSf <vmm-release-url>/valheim-mod-manager-installer.sh | VALHEIM_MOD_MANAGER_INSTALL_DIR=/usr/local/bin sh && curl --proto "=https" --tlsv1.2 -LsSf https://raw.githubusercontent.com/<your-fork>/valheim-mod-manager/master/scripts/vmm-update-and-restart.sh -o /usr/local/bin/vmm-update-and-restart.sh && chmod +x /usr/local/bin/vmm-update-and-restart.sh' \
   -e POST_UPDATE_CHECK_HOOK='/usr/local/bin/vmm-update-and-restart.sh'
   ```

   `POST_BOOTSTRAP_HOOK` runs once after bootstrap, before any service
   starts. The first `curl` fetches vmm's own prebuilt static binary onto
   the container's `PATH` using its cargo-dist installer script; no compiler
   needed in the image. The second `curl` fetches
   [`scripts/vmm-update-and-restart.sh`](scripts/vmm-update-and-restart.sh)
   from the repo and installs it alongside `vmm`. Both run on every
   bootstrap, so there is no separate manual copy step and nothing to keep
   in sync with the persistent `/config` volume; if you'd rather not depend
   on a network fetch for the script, copy it onto `/config` yourself instead
   and point `POST_UPDATE_CHECK_HOOK` at that path.

   `POST_UPDATE_CHECK_HOOK` runs on the image's existing `valheim-updater`
   schedule (`UPDATE_CRON`, default `*/15 * * * *`). The script first creates
   `game_dir`'s `BepInEx/{plugins,patchers,config}` symlinks into
   `/config/bepinex` if they are missing or wrong (idempotent, so this costs
   nothing on ordinary runs), then runs `vmm update manifest && vmm
   update mods`, then, if something actually changed, runs the image's own
   `sync_bepinex_loadables` to copy `/config/bepinex` into the live install
   and restarts the `valheim-server` process via `supervisorctl`. The sync
   matters: the image only does that copy at container boot and during its own
   BepInEx updates, so a bare server restart would come back up on the old
   mods (removed mods stay loaded, version changes never apply). It compares `vmm list
   --format json` before and after, trimmed to the JSON payload itself:
   `vmm`'s own tracing output goes to stdout too and is timestamped, so a
   raw capture would report a change on every run.

3. **Install your mods once against the container.** The BepInEx install and
   everything inside it is root-owned, and every process that touches it
   (SteamCMD, `valheim-updater`, the game server itself) runs as root inside
   the container, so `vmm` has to run there too, not from the host against
   the bind-mounted directory. Use `docker exec` to run it, or `docker exec
   -it valheim-server sh` to open a shell and run it directly from there:

   ```bash
   docker exec -i valheim-server vmm --config /config/vmm_config.toml import \
     a1b2c3d4-0000-0000-0000-000000000000
   # or a file export
   docker exec -i valheim-server vmm --config /config/vmm_config.toml import \
     /path/to/exported.r2z
   # or naming mods directly
   docker exec -i valheim-server vmm --config /config/vmm_config.toml install \
     Owner-Mod1 Owner-Mod2
   ```

   This is the same imperative, `mods.yml`-backed install flow as a local
   `vmm install`/`vmm import`; nothing Docker-specific about it. A mod that
   exists only on Hexium resolves automatically once `hexium` is in
   `[sources] enabled`. If you're migrating
   mods that were previously placed by hand, `install` puts each one at its
   **latest** version, not whatever was there before; check your old versions
   first if any were intentionally held back. Restart once
   afterward to confirm the server loads cleanly from `vmm`'s install.

4. **Limits of this approach.**

     Every mod that depends on the BepInEx loader re-triggers the loader's
     own dependency resolution on every `update mods` run, so `vmm` reinstalls
     it into `game_dir` alongside the mods that need it. `BepInEx/core` is
     not one of the symlinked routes, though, so that copy lands in
     `/config/vmm_game/BepInEx/core` and never reaches the image's live
     install. This is expected: `BEPINEX=true` already fully owns installing
     and updating the actual loader (it's what sets up the doorstop
     `LD_PRELOAD` hook, so it cannot be turned off), so `vmm`'s copy is left
     inert on purpose rather than competing with it for writes to the same
     live location.

     `BepInEx/monomod` (MonoMod hook DLLs, used by a small minority of mods)
     is not symlinked either, for a different reason: the image's own sync
     only ever copies `plugins/` and `patchers/` out of `/config/bepinex`.
     Even a mod placed there entirely by hand, with no `vmm` involved, would
     not reach the live install. This is a pre-existing limitation of the
     image itself. It is not something this integration introduces, and it
     cannot be worked around without symlinking a folder the image's own
     sync never reads.

## Troubleshooting

If you encounter issues, increase log verbosity in your config:

```toml
log_level = "debug"
```

Then run the command again to see more detailed output.

## License

This project is licensed under the MIT License - see the LICENSE file for details.

## Acknowledgments

- [Thunderstore](https://thunderstore.io) for hosting Valheim mods
- [Hexium](https://hexium.gg) for hosting Valheim mods
- The amazing Valheim modding community
