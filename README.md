# gphoto2immich

Local desktop daemon that watches for a USB-connected camera and
auto-uploads new photos into an [Immich][] instance. JPEG+RAF pairs are
stacked. Deduplication uses Immich's metadata search — no local database.

Single-camera, single-Immich. Built around [libgphoto2][], so in principle
it should work for any camera libgphoto2 supports. The camera's EXIF
`Make` tag is auto-detected at session start (so the Immich dedup scope
isn't hardcoded), but the stacking logic is still JPEG+RAF-shaped —
fine on Fuji bodies, would need a small tweak on Canon/Nikon/etc. for
their CR2/NEF flavour of RAW+JPEG. It's been shaped against a Fujifilm
X-T3.

[Immich]: https://immich.app
[libgphoto2]: http://gphoto.org/proj/libgphoto2/

## How it works

1. Polls libgphoto2 every few seconds for a connected camera.
2. When one is detected, runs a single sync sweep:
   - Enumerates the camera's filesystem (`/store_*/DCIM/...`).
   - Reads the EXIF `Make` tag from the first JPEG on the card via
     `download_exif` (~65 KB, in-memory) so the Immich query can scope
     to one manufacturer.
   - Builds a per-session in-memory snapshot of all matching Immich
     assets in one paginated search. This replaces what would otherwise
     be N HTTP requests during the per-file pre-check.
   - Cutoff for the backfill is the newest asset in the snapshot minus
     1h slop. Files older than that are skipped (assumed already
     uploaded).
   - For each remaining file: HashMap lookup against the snapshot
     (filename + ±24h `fileCreatedAt` window). If found, records the
     existing asset id for stacking and skips the download. Otherwise
     downloads, hashes, and POSTs to `/api/assets` with
     `x-immich-checksum` so Immich also dedups server-side.
3. JPEG+RAF pairs (same basename, both seen during the session — fresh
   or pre-existing) get stacked via `/api/stacks`. The JPEG is primary.
4. Session ends once the walk completes. Daemon waits for the camera to
   be unplugged before considering another sync against the same port —
   no event-loop watching for shutter presses while tethered.

## Requirements

- Linux desktop (D-Bus session for the optional notification popup).
- `libgphoto2` (Arch: `pacman -S libgphoto2`). On Debian/Ubuntu install
  `libgphoto2-6` plus `libgphoto2-dev` if building from source.
- Rust toolchain (stable) if you're building from source.

## Configuration (env vars)

| Var               | Required | Default | Notes                                              |
| ----------------- | -------- | ------- | -------------------------------------------------- |
| `IMMICH_URL`      | yes      | —       | e.g. `https://immich.example.com/`                 |
| `IMMICH_API_KEY`  | yes      | —       | `x-api-key` header value. See [API key permissions](#api-key-permissions) below for a minimum-scope key. |
| `TZ`              | yes      | —       | IANA TZ the camera's clock is set to (e.g. `Europe/Amsterdam`). libgphoto2 reports mtime as camera-local wall-clock reinterpreted as Unix epoch, so we need this to get true UTC. Must be an IANA name — POSIX TZ strings like `CET-1CEST,M3.5.0,M10.5.0/3` aren't accepted. Often already set in your shell / systemd user env. |
| `STACK_JPEG_RAF`  | no       | `true`  | Stack JPEG+RAF pairs via Immich's stacks API.      |
| `LOG_LEVEL`       | no       | `info`  | `tracing_subscriber` env filter (`debug`, `trace`…). |

## API key permissions

The daemon needs **three** Immich permissions on its API key — no need
to hand it a full-access key:

| Permission     | Why we need it                                                  |
| -------------- | --------------------------------------------------------------- |
| `asset.read`   | `POST /api/search/metadata` (Immich cache snapshot at session start) + `GET /api/assets/{id}` (checking whether an existing JPEG is already in a stack before we'd create one) |
| `asset.upload` | `POST /api/assets` (the multipart upload itself)                |
| `stack.create` | `POST /api/stacks` (creating a JPEG+RAF stack)                  |

In the Immich web UI: **Account Settings → API Keys → New API Key**,
untick `all`, tick those three. Set `STACK_JPEG_RAF=false` in the
env if you also want to drop the `stack.create` requirement.

## Running

```sh
IMMICH_URL=https://immich.example.com/ \
IMMICH_API_KEY=… \
TZ=Europe/Amsterdam \
cargo run --release
```

The daemon prints what it's doing to stderr. When a camera comes up
you should see `camera detected model=...` followed by
`detected camera EXIF make make=...`, then the backfill summary
line at the end. Unplug the camera (or stop the daemon) when done — it
doesn't watch for shutter presses while tethered.

## Camera-side caveats

### Fujifilm X-T3 (and similar bodies)

The X-T3 has to be in a PTP USB mode, not Card Reader. The menu path is

```
MENU/OK → SET UP → CONNECTION SETTING → PC CONNECTION MODE → "USB Tether Shooting Auto/Fixed"
```

In Card Reader mode the camera presents as USB Mass Storage and libgphoto2
ignores it. The CLI sanity check is `gphoto2 --auto-detect` — if that
doesn't see the camera, neither will gphoto2immich.

### gvfs auto-mount conflict (most desktop installs)

On GNOME / KDE / anything with `gvfs-gphoto2-volume-monitor` installed, the
desktop will autoclaim the camera the instant you plug it in, and
gphoto2immich will see `IoUsbClaim` errors. Easiest fix:

```sh
systemctl --user mask gvfs-gphoto2-volume-monitor.service
```

(or `pkill -f gvfs-gphoto2-volume-monitor` for a one-off.)

## Running as a service

The repo ships a systemd **user** unit. Install + enable:

```sh
mkdir -p ~/.config/gphoto2immich ~/.config/systemd/user
cp packaging/systemd/env.example ~/.config/gphoto2immich/env
chmod 600 ~/.config/gphoto2immich/env
$EDITOR ~/.config/gphoto2immich/env   # fill in IMMICH_URL, IMMICH_API_KEY, TZ
cp packaging/systemd/gphoto2immich.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now gphoto2immich.service
journalctl --user -u gphoto2immich -f
```

It restarts on failure (10s backoff). Notifications work because the user
session has a D-Bus address; running it as a system service won't pop up
notifications.

## Arch package (PKGBUILD)

`packaging/arch/PKGBUILD` builds a `-git` package from the repo:

```sh
cd packaging/arch
makepkg -si
```

That puts the binary at `/usr/bin/gphoto2immich` and the systemd user unit at
`/usr/lib/systemd/user/gphoto2immich.service`. Per-user setup is still the env
file at `~/.config/gphoto2immich/env`.

## Notifications

Three desktop notification types via libnotify:

- **Camera connected** — fires once when a camera is detected.
- **Sync complete** — at session end, with the count of newly-uploaded
  assets. Suppressed if zero (the connect popup already told you
  something happened).
- **Sync failed** — when a session ends in error, with a truncated
  error chain in the body.

Notifications are deduped per plug-in cycle: if a session keeps failing
and retrying every few seconds (e.g. Immich unreachable), you get one
"Camera connected" + one "Sync failed" popup, not one per retry. The
flags reset when the camera unplugs.

Nothing per file — the log is the source of truth for per-file detail.
Notifications are best-effort: if D-Bus isn't available, the daemon
logs the failure at debug level and keeps running.

## Development

```sh
cargo test          # unit tests, mostly Immich client + cache + stack tracker
cargo run           # against your real camera; needs the env vars above
cargo clippy --all-targets --no-deps -- -D warnings
cargo fmt --all -- --check
```

GitHub Actions runs all four on every push and PR — see
`.github/workflows/ci.yml`.

There's no integration test against a real camera (or vcam — there
isn't a libgphoto2 equivalent that's easy to wire up). The Immich
client is covered by [wiremock][] tests; the rest needs hardware.

[wiremock]: https://crates.io/crates/wiremock

## Known limitations

### Cutoff and re-uploading deleted assets

The backfill skips any file on the card older than `most_recent_immich_asset_of_the_same_make.fileCreatedAt − 1h`. That's a one-line optimisation that keeps the backfill linear in the diff between card and Immich, not in card size — but it has a sharp edge:

> If you delete a file from Immich and want it re-uploaded, the cutoff will skip it unless every Immich asset newer than it is also gone.

In practice this means deleting one or two recent files from the UI and re-plugging the camera won't always pull them back. Either delete the *newest* asset too (so the cutoff drops below the file you actually want restored), or re-upload it manually via the Immich web UI by dragging from the card.

### What it intentionally doesn't do

- Network/Wi-Fi sync. USB only.
- Multi-camera support. Single body, single instance.
- Local SQLite / state cache. The Immich-side snapshot is rebuilt from scratch each session.
- Watching for new shots while the camera is tethered. The daemon does one sync sweep on plug-in, then waits for unplug before considering another. If you want a continuously-watching workflow, unplug + replug after each shot, or just rely on libgphoto2's auto-detect being fast (~3s).
- **Delete from the camera after upload. Photos stay on the card** — deliberate. The card is your backup until you decide it isn't. Format when you're ready.

## License

[AGPL-3.0-or-later](LICENSE).
