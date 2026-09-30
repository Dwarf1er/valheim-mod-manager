<div align="center">


# vsmm
##### Keep a Valheim dedicated server's mods in sync with a Gale profile

<img src="assets/logo.svg" alt="vsmm logo" width="160">

![License](https://img.shields.io/github/license/Dwarf1er/valheim-server-mod-manager?style=for-the-badge)
![Issues](https://img.shields.io/github/issues/Dwarf1er/valheim-server-mod-manager?style=for-the-badge)
![PRs](https://img.shields.io/github/issues-pr/Dwarf1er/valheim-server-mod-manager?style=for-the-badge)
![Contributors](https://img.shields.io/github/contributors/Dwarf1er/valheim-server-mod-manager?style=for-the-badge)
![Stars](https://img.shields.io/github/stars/Dwarf1er/valheim-server-mod-manager?style=for-the-badge)

</div>

`vsmm` is a mod manager for Valheim **dedicated servers**, built to run inside the [community Valheim server Docker image](https://github.com/community-valheim-tools/valheim-server-docker). Point it at a [Gale profile sync](https://github.com/Kesomannen/gale-sync) id and the server follows that profile on its own: mods are installed, removed, and held at the versions the profile names, and the server restarts only when something actually changed. Profile sync is the headline feature, but not the only way in: you can also import a profile code, an `.r2z` file, or an r2modman or Gale profile folder, or install mods by hand (see [Ways to manage mods](#ways-to-manage-mods)).

It's a hard fork of [Endoze/valheim-mod-manager](https://github.com/Endoze/valheim-mod-manager), a desktop client, and this fork shaped it to specifically be a server-side tool. A huge thank you to [Endoze](https://github.com/endoze) for all the amazing work on `vmm` without which this project wouldn't exist. This truly is a modification built entirely upon the massive work that went into `vmm`.

Mods come from [Thunderstore](https://thunderstore.io) and [Hexium](https://valheim.hexium.gg).

## Table of Contents

<!-- mtoc-start -->

* [Quickstart](#quickstart)
* [Why this fork](#why-this-fork)
* [How it works](#how-it-works)
* [Ways to manage mods](#ways-to-manage-mods)
  * [Applying changes right away](#applying-changes-right-away)
* [Setup](#setup)
* [Configuration](#configuration)
* [Version pinning](#version-pinning)
* [Commands](#commands)
* [Under the hood](#under-the-hood)
  * [Where mods come from](#where-mods-come-from)
  * [Files vsmm keeps](#files-vsmm-keeps)
  * [The Docker integration](#the-docker-integration)
* [Troubleshooting](#troubleshooting)
* [Building from source](#building-from-source)
* [Acknowlegements](#acknowlegements)
* [License](#license)

<!-- mtoc-end -->

## Quickstart

Already running the [community Valheim server image](https://github.com/community-valheim-tools/valheim-server-docker) with `BEPINEX=true`? Three steps and your server follows a Gale profile.

**1. Add a config file** named `vsmm_config.toml` to the folder you mount at `/config`, using the short code Gale gives you when you sync a profile (for example `8GWAV8`):

```toml
game_dir = "/config/vsmm_game"
data_dir = "/config/vsmm"

[sources]
enabled = ["hexium", "thunderstore"]

[gale_sync]
profile_id = "YOUR-PROFILE-SYNC-ID"
```

The container creates that folder as root, so a normal user can't write to it. Either use `sudo` (for example `sudo nano config/vsmm_config.toml`), or save the file anywhere and copy it in through the container:

```bash
docker compose exec -T valheim sh -c 'cat > /config/vsmm_config.toml' < vsmm_config.toml
```

**2. Add these to your compose file**, under the server's `environment:`:

```yaml
    environment:
      BEPINEX: "true"
      POST_BOOTSTRAP_HOOK: curl --proto "=https" --tlsv1.2 -LsSf https://github.com/Dwarf1er/valheim-server-mod-manager/releases/latest/download/valheim-server-mod-manager-installer.sh | VALHEIM_SERVER_MOD_MANAGER_UNMANAGED_INSTALL=/usr/local/bin sh && curl --proto "=https" --tlsv1.2 -LsSf https://raw.githubusercontent.com/Dwarf1er/valheim-server-mod-manager/master/scripts/vsmm-update-and-restart.sh -o /usr/local/bin/vsmm-update-and-restart.sh && chmod +x /usr/local/bin/vsmm-update-and-restart.sh
      POST_UPDATE_CHECK_HOOK: /usr/local/bin/vsmm-update-and-restart.sh
```

Using `docker run` instead? Pass the same three as `-e NAME='value'` flags.

The hook has to run as root to install `vsmm` into `/usr/local/bin`, so leave `PUID` and `PGID` unset (or set both to `0`).

**3. Recreate the container and run the update script:**

```bash
docker compose up -d
# wait a few seconds for the container to finish starting, then:
docker compose exec valheim /usr/local/bin/vsmm-update-and-restart.sh
```

Replace `valheim` with your service name. The script syncs your profile, copies the mods into the live server and restarts it. The image also runs this same script by itself shortly after startup and then on the image's update schedule (every 15 minutes by default, and only while no players are connected), so running it by hand is optional; it just applies everything now instead of waiting. From here on, every cycle re-checks the profile, installs and removes what changed, and restarts the server only if something did.

Mods you placed in `/config/bepinex` by hand are left where they are. `vsmm` only manages what it installed itself. Starting from scratch, want to skip the profile, or curious what the script does? See [Setup](#setup), [Ways to manage mods](#ways-to-manage-mods) and [Applying changes right away](#applying-changes-right-away).

## Why this fork

The original [valheim-mod-manager](https://github.com/Endoze/valheim-mod-manager), by Endoze, is an incredible desktop cli mod manager, and everything this fork does with mods is built on their work. This fork wanted that same engine for a different job: a dedicated server in a Docker container, where nobody is at the keyboard, there's no Steam client, and the mod list should simply follow whatever the admins decide.

Serving both a desktop and a server well would have meant carrying a lot of options that get in each other's way, so this fork went its own direction and set aside the parts a headless server doesn't need. `vmm` is truly an amazing tool, we just needed to tweak it a little bit for our own usecase!

**Set aside:**

- `launch` and the Steam/Proton machinery around it.
- Named profiles and the `--profile` flags. There is one target, `game_dir`.
- `migrate` and the old `mod_list` / `install_dir` config keys. This fork starts from a clean config, so there's no upgrade path from the desktop tool's config.
- The Windows and macOS builds. Releases are a single static amd64 Linux binary.

**Added:**

- `vsmm sync`, which follows a [Gale profile sync](https://github.com/Kesomannen/gale-sync) id and is checked on every update cycle.
- Reconciliation by default: imports and sync uninstall what the source no longer lists (`--additive` opts out), and refuse to do so against an empty list.
- Version pinning, so the server runs the versions a profile names instead of drifting to latest (`track_latest` opts out).
- The hook script that ties it to the image's update schedule, copies changes into the live install, and restarts the server only when something changed.
- Hexium as a second mod source, resolved alongside Thunderstore.

Mod resolution, installing, and the `mods.yml` record still come from [thunderstore-engine](https://github.com/Endoze/thunderstore-engine), used unchanged as a normal dependency.

## How it works

The image already runs an update check on a schedule and lets you hook into it. By default it checks every 15 minutes (`UPDATE_CRON='*/15 * * * *'`), and only when no players are connected (`UPDATE_IF_IDLE=true`). `vsmm` plugs into that hook:

```
Gale profile sync -> vsmm sync -> /config/bepinex -> image sync -> live server
   (the truth)      install, prune,   (persistent)    (rsync into      restarted only
                    pin to versions                    /opt/valheim)    if mods changed
```

Every cycle, `scripts/vsmm-update-and-restart.sh` does the following:

1. Refreshes the package indexes.
2. If a Gale profile is configured, runs `vsmm sync`: fetches it, installs what it names at the versions it names, and uninstalls anything it no longer lists.
3. Runs `vsmm update mods`: brings unpinned mods to their latest versions (pinned ones stay put).
4. If the installed mods changed, copies them into the live install and restarts the game server. Otherwise it does nothing.

If Gale is unreachable, the cycle keeps whatever is installed and carries on. A server never fails to boot because a web service was down.

Because the hook only runs when the image's update check does, mod changes wait for the server to be empty and never kick anyone off; on a busy server, changes wait until a check happens to find it empty. You can change how often it runs by setting `UPDATE_CRON` on the container (for example `'0 6 * * *'` for once a day at 6 AM), or set `UPDATE_IF_IDLE=false` to run even with players connected. Setting `UPDATE_CRON` to an empty string turns the schedule off entirely, and then the hook only runs at startup.

## Ways to manage mods

Gale profile sync is one of several ways to tell `vsmm` what the server should run. All of these work inside the container (`docker exec -i valheim vsmm --config /config/vsmm_config.toml …`):

| You have | Run | What happens |
|---|---|---|
| A Gale profile sync id | `vsmm sync` (or set `profile_id` and let the schedule do it) | Installs what the profile names at its versions, removes what it doesn't list, and keeps following it |
| A profile code (Thunderstore's, or a Gale one, including Hexium-only mods) | `vsmm import <code>` | One-time import at the listed versions; removes what the code doesn't list |
| An `.r2z` export file | `vsmm import ./profile.r2z` | Same as a profile code |
| An r2modman profile folder | `vsmm import <folder>` | Adopts the folder as it is, at the versions it recorded |
| A live Gale profile folder | `vsmm import <folder>` | Reinstalls every mod it finds, at latest (Gale records no versions on disk) |
| A few mods you want | `vsmm install Owner-Mod …` | Installs them (and their dependencies) at latest |

**Using them alongside profile sync.** If `[gale_sync] profile_id` is set, every scheduled cycle reconciles the server to that profile, so anything added by hand or by another import that the profile doesn't list will be removed on the next cycle. That's intentional: the profile is the source of truth.

If you'd rather manage the server with imports and `install` instead, leave `profile_id` unset. The scheduled cycle then only runs `vsmm update mods`: unpinned mods move to latest, pinned ones (from an import) stay put, and nothing is ever pruned. The script logs that sync isn't configured on each cycle; that's expected.

### Applying changes right away

`vsmm sync`, `import` and `install` change what `vsmm` has installed (in `/config/bepinex`), but the running server only sees it once the update script copies it into the live install and restarts the game server. That happens on the next scheduled cycle. To force it now, run the script:

```bash
docker exec -i valheim /usr/local/bin/vsmm-update-and-restart.sh
```

This is the same thing the schedule runs, and it does the whole job in one go: refreshes the package indexes, syncs the profile (if `profile_id` is set), updates unpinned mods, and, if the installed mods differ from what the live server was last given, copies them in and restarts the game server. If nothing differs it does nothing, so it's safe to run any time. It does not wait for an empty server, though: unlike the scheduled run, a manual run restarts the game server right away even if players are connected.

Changes you made by hand count too: the script compares against what the live server last received, not against the start of its own run. So `vsmm import …` or `vsmm install …` followed by the script gets those mods onto the server, and the next scheduled cycle would have done the same.

Two notes:

- If you only want to update what `vsmm` has installed without touching the running server, run `vsmm sync` (or `vsmm update mods`) on its own. The server picks it up at the next cycle.
- With `profile_id` set, the script reconciles to the profile first, so a mod you just added by hand that the profile doesn't list is removed again by that same run. To keep hand-added mods, leave `profile_id` unset.

## Setup

**1. Config file.** Create `vsmm_config.toml` in your `/config` volume:

```toml
game_dir = "/config/vsmm_game"
data_dir = "/config/vsmm"

[sources]
enabled = ["hexium", "thunderstore"]

# Optional: leave this out to manage mods with `vsmm import` / `vsmm install` instead
[gale_sync]
profile_id = "YOUR-PROFILE-SYNC-ID"
```

Keep `log_level` unset (it defaults to `error`). See [Troubleshooting](#troubleshooting) for why.

List `hexium` first if you use it: Gale uploads profile codes that contain Hexium-only mods to Hexium's endpoint, and `vsmm` tries sources in this order.

**2. Container.** Add two environment variables to the image. They install `vsmm` and the hook script at each boot, then run the script on the image's update schedule:

```yaml
services:
  valheim:
    image: ghcr.io/community-valheim-tools/valheim-server
    ports:
      - "2456-2457:2456-2457/udp"
    volumes:
      - ./config:/config
      - ./data:/opt/valheim
    environment:
      BEPINEX: "true"
      SERVER_NAME: "My Server"
      WORLD_NAME: "MyWorld"
      SERVER_PASS: "change-me"
      POST_BOOTSTRAP_HOOK: curl --proto "=https" --tlsv1.2 -LsSf https://github.com/Dwarf1er/valheim-server-mod-manager/releases/latest/download/valheim-server-mod-manager-installer.sh | VALHEIM_SERVER_MOD_MANAGER_UNMANAGED_INSTALL=/usr/local/bin sh && curl --proto "=https" --tlsv1.2 -LsSf https://raw.githubusercontent.com/Dwarf1er/valheim-server-mod-manager/master/scripts/vsmm-update-and-restart.sh -o /usr/local/bin/vsmm-update-and-restart.sh && chmod +x /usr/local/bin/vsmm-update-and-restart.sh
      POST_UPDATE_CHECK_HOOK: /usr/local/bin/vsmm-update-and-restart.sh
```

`BEPINEX=true` is required. `vsmm` installs mods, but the image is what loads BepInEx.

**Leave `PUID` and `PGID` unset, or set both to `0`.** They set the user the image runs as, and any other value (such as `1000`) runs the hooks as an unprivileged user. The install then fails with `mktemp: failed to create directory via template '/usr/local/bin/tmp.XXXXXXXXXX': Permission denied`, because `vsmm` and its update script need root to write to `/usr/local/bin`, sync mods into the live install and restart the server. The trade-off is that `./config` on the host is root-owned, so use `sudo` to edit files in it.

If you write `environment:` as a list (`- NAME=value`) instead of a map, keep `POST_BOOTSTRAP_HOOK` on one line, as shown above. A list entry can't span multiple lines.

**3. First run.** `vsmm` only appears on the container's `PATH` after the first boot with the hook. Start the container, give it a few seconds, then apply everything now (or, with `profile_id` set, just wait for the next scheduled cycle):

```bash
docker exec -i valheim /usr/local/bin/vsmm-update-and-restart.sh
```

To load mods a different way first, run the command from [Ways to manage mods](#ways-to-manage-mods), for example `docker exec -i valheim vsmm --config /config/vsmm_config.toml import <profile-code>`, and then run the script above to put them on the server.

`vsmm` has to run **inside** the container, not from the host against the bind mount: everything it touches is root-owned. After this, the schedule takes over.

## Configuration

The config file is read from `vsmm_config.toml` in the working directory and/or the user config directory (`~/.config/vsmm/`); the local file wins key by key. `--config <path>` bypasses that lookup. In the container it's `/config/vsmm_config.toml`.

| Key | Default | Meaning |
|---|---|---|
| `game_dir` | (required) | The directory `vsmm` installs into. In the container, `/config/vsmm_game`. `~` and `$VARS` are expanded. `vsmm` refuses to run if it doesn't exist rather than creating a mod tree somewhere else. |
| `data_dir` | `~/.config/vsmm` | Package cache and exports. In the container, `/config/vsmm`, so it survives recreation. |
| `log_level` | `error` | `error`, `warn`, `info`, `debug`, or `trace`. |
| `track_latest` | `false` | Ignore versions named by lists and keep everything on its latest. See [Version pinning](#version-pinning). |
| `[sources] enabled` | `["thunderstore"]` | Which mod sources to use, in tie-break order. Add `"hexium"` to use [Hexium](https://valheim.hexium.gg) too. |
| `[gale_sync] profile_id` | unset | The Gale profile sync id `vsmm sync` follows. Unset disables it. Reading a profile needs no token, so none is stored. |
| `[gale_sync] base_url` | `https://gale.kesomannen.com/api` | The gale-sync API root. |

## Version pinning

Gale profiles (and `.r2z` files, profile codes, and r2modman `mods.yml` files) name a version for every mod, dependencies included. By default `vsmm` respects them:

- `import` and `sync` install each mod at its listed version and remember it as a **pin**.
- `update mods` keeps pinned mods at their pinned version and moves everything else to latest.
- `vsmm install X` by hand installs X at its latest version and drops X's pin. Already-pinned dependencies stay pinned; new ones come in at latest.
- If a listed version is no longer offered by any configured source, that mod is skipped with a warning and whatever is installed stays as it is.
- A live Gale profile *directory* records no versions, so it installs latest and pins nothing.
- If a package is on several sources, the source that has the pinned version wins, even if another is newer.

Set `track_latest = true` to ignore listed versions entirely: nothing is pinned and everything goes to latest.

Auto-updating a modded server unattended can pull in an update that breaks compatibility with your players' clients. Pinning is what keeps the server on the versions your profile says.

## Commands

```bash
# Follow the configured Gale profile (install, prune, pin)
vsmm sync

# Install mods and their dependencies (at latest)
vsmm install denikson-BepInExPack_Valheim ValheimModding-Jotunn

# See what is installed
vsmm list
vsmm list --format json

# Update every unpinned mod (pinned mods stay), refresh the package indexes
vsmm update mods
vsmm update manifest

# Take a mod out of play without uninstalling it
vsmm disable ValheimModding-Jotunn
vsmm enable ValheimModding-Jotunn
vsmm disable --all           # every mod except the loader

# Remove mods
vsmm uninstall ValheimModding-Jotunn
vsmm uninstall --force ValheimModding-Jotunn   # also delete unrecorded folders
vsmm uninstall --all [--yes]                   # everything, loader included

# Search every configured source (or one)
vsmm search jotunn
vsmm search --source hexium jotunn

# Import from an .r2z file, a profile code, or a profile directory
vsmm import ./profile.r2z
vsmm import a1b2c3d4-0000-0000-0000-000000000000
vsmm import --additive ./profile.r2z   # keep mods the source no longer names

# Share what is installed
vsmm export            # writes an .r2z under data_dir
vsmm export --code     # uploads and prints a profile code
```

**Importing is authoritative.** After installing, a file or profile-code import (and `sync`) uninstalls anything the source no longer names, so the server converges on the source. Use `--additive` to only add. Directory imports (r2modman or Gale profile folders) are a raw copy and never prune. `vsmm` also refuses to prune against a source that names no mods, since that would uninstall everything, the mod loader included.

**`mods.yml` owns the whole mod tree.** An uninstall reconciles every `Owner-ModName` folder under the install routes against `mods.yml`, so a folder you dropped in by hand would be removed too. `vsmm uninstall` therefore checks first: if it finds folders `mods.yml` doesn't record, it names them and refuses. Bring them under management with `vsmm install`, or pass `--force` to delete them along with the named mods. Loader folders such as `BepInEx/plugins/MMHOOK` and anything directly under `BepInEx/config/` are never touched.

**`update mods` reinstalls everything it manages** on every run, so any file a mod ships inside its own plugin folder is overwritten, including hand edits to it. Config files placed directly under `BepInEx/config/` are left alone. Mods you disabled stay disabled.

## Under the hood

### Where mods come from

With more than one source configured, `vsmm` merges their package indexes into one before resolving anything:

1. Each source's index is fetched and cached under `data_dir`.
2. When the same package (`Owner-ModName`) exists on several sources, the one with the later `date_updated` wins. The order of `enabled` only breaks exact ties. A mod on only one source is unaffected. Pins change this rule slightly: a source that offers the pinned version beats one that does not.
3. The full dependency closure of what you asked for is resolved against that one merged index.
4. Packages are downloaded and extracted into a shared cache, then installed using the mod loader's install rules.

This is why Hexium-only mods work everywhere `vsmm` names a mod: install, update, and every import route. `vsmm search --source` only filters what is displayed; it doesn't change how a mod resolves.

### Files vsmm keeps

Inside `game_dir`:

- **`mods.yml`** is the record of what is installed. It uses the r2modman/Gale-compatible format, one entry per mod with its version, dependencies, and enabled state. The engine owns and rewrites it; it is what `list`, `uninstall`, `enable`/`disable`, and `export` read.
- **`.vsmm_state.json`** holds what `mods.yml` has no room for: where each mod came from, and the version it is pinned to.

  ```json
  {
    "Advize-PlantEverything": { "source": "thunderstore", "pin": "1.21.2" },
    "Azumatt-AAA_Crafting":   { "source": "hexium",      "pin": "2.1.10" }
  }
  ```

  A mod with no `pin` tracks latest. Entries disappear when their mod is uninstalled.

Inside `data_dir/valheim/`: the downloaded package cache and `.r2z` exports.

### The Docker integration

The engine always installs into `<game_dir>/BepInEx/{plugins,config,patchers}`, and neither of the image's own BepInEx paths has that shape. `/opt/valheim/bepinex` is the image's live install, rebuilt from scratch on every game or BepInEx update, so anything written there would be wiped. `/config/bepinex` is the persistent volume but is flattened (`plugins/` directly under it). So `game_dir` points at a small directory `vsmm` owns, `/config/vsmm_game`, whose `BepInEx/` folders are symlinks:

```
/config/vsmm_game/BepInEx/plugins  -> /config/bepinex/plugins
/config/vsmm_game/BepInEx/patchers -> /config/bepinex/patchers
/config/vsmm_game/BepInEx/config   -> /config/bepinex
```

The hook script (re)creates these on every run. `vsmm` writes through them as if they were an ordinary game directory, and everything lands in `/config/bepinex`: the same folder the image tells you to put mods in by hand. A mod's own `.cfg` file, written the first time it runs, lands directly in `/config/bepinex/`, which is where to look to edit its settings.

The image copies `/config/bepinex` into the live install only at container boot and during its own BepInEx updates, not when the game server process restarts. So before restarting, the hook script runs the image's own `sync_bepinex_loadables`. Without it, removed mods would stay loaded and version changes would never apply. The sync deletes a removed mod's files but leaves its empty folder in the live install, which is harmless.

**Limits.**

- `BepInEx/core` (the loader itself) is not symlinked. Mods that depend on the loader make `vsmm` reinstall it into `game_dir`, and that copy stays in `/config/vsmm_game/BepInEx/core` and never reaches the live install. This is intentional: `BEPINEX=true` owns installing and updating the real loader, so `vsmm`'s copy is left inert rather than competing with it.
- `BepInEx/monomod` (MonoMod hook DLLs, used by a few mods) is not symlinked either. The image's sync only copies `plugins/` and `patchers/`, so even a hand-placed file there would not reach the live install. This is a limitation of the image, not something the integration adds.
- Dependencies a list doesn't name are not pinned, because the engine has no version-constraint solving. They resolve to latest. Gale and r2modman lists include every dependency, so this only affects hand-written lists.

## Troubleshooting

To see what `vsmm` is doing, run it once with a debug config. Don't change the container's own config for this (see below):

```bash
docker exec -i valheim sh -c 'printf "log_level = \"debug\"\n" > /tmp/vsmm_debug.toml; grep -v "^log_level" /config/vsmm_config.toml >> /tmp/vsmm_debug.toml; vsmm --config /tmp/vsmm_debug.toml sync'
```

**Do not leave `log_level` at `info` or lower in the container config.** At `info`, each run prints about 1,300 lines about which source won for each shared mod. In testing, running that on a schedule backed up the image's `supervisord`/`syslogd` output until `supervisorctl` hung. The game server kept running, but supervision didn't. The default (`error`) is what the container should use.

**A mod I removed from the profile is still loaded.** Check that the hook ran the live sync: the script logs "syncing them into the live install" whenever mods changed. If you're running an older copy of the script, restart the container so the boot hook fetches the current one. To apply changes immediately instead of waiting for the next cycle, see [Applying changes right away](#applying-changes-right-away).

**A mod is skipped with "pinned to X but no configured source offers it".** The version your profile names is gone from every source in `[sources] enabled`. The installed version is left alone. Either update the profile, or set `track_latest = true`.

## Building from source

```bash
git clone https://github.com/Dwarf1er/valheim-server-mod-manager.git
cd valheim-server-mod-manager
cargo build --release      # binary at target/release/vsmm
cargo test
```

Requires Rust 1.94 or newer. Release builds are produced by [cargo-dist](https://opensource.axo.dev/cargo-dist/) when a version tag is pushed.

## Acknowlegements

- [Endoze/valheim-mod-manager](https://github.com/Endoze/valheim-mod-manager), the project this was forked from, and [thunderstore-engine](https://github.com/Endoze/thunderstore-engine), the library that does the installing.
- [Gale](https://gale.kesomannen.com/) and [gale-sync](https://github.com/Kesomannen/gale-sync) for the profile sync this is built around.
- [Thunderstore](https://thunderstore.io) and [Hexium](https://valheim.hexium.gg) for hosting Valheim mods.
- [community-valheim-tools/valheim-server-docker](https://github.com/community-valheim-tools/valheim-server-docker) for the server image.

## License

This software is licensed under the [MIT license](LICENSE).
