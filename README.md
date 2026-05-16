# Fujimmich

Local desktop daemon that watches for a USB-connected Fujifilm camera and
auto-uploads new photos into an [Immich][] instance. JPEG+RAF pairs are
stacked. Deduplication uses Immich's metadata search — no local database.

Single-camera, single-Immich. Built around [libgphoto2][], so in practice it
should work for any camera libgphoto2 supports; it's been built and shaped
against an X-T3.

[Immich]: https://immich.app
[libgphoto2]: http://gphoto.org/proj/libgphoto2/

## How it works

1. Polls libgphoto2 every few seconds for a connected camera.
2. When one is detected:
   - Asks Immich for the most recent uploaded asset and uses its capture
     time (minus 1h slop) as a cutoff for the initial backfill.
   - Walks the camera's filesystem; for every file newer than the cutoff,
     does a metadata pre-check against Immich (`originalFileName` + ±2 min
     window). If it's already known, records the existing asset id for
     stacking and skips the download.
   - Otherwise downloads it, hashes it on the fly, and POSTs to
     `/api/assets` with the `x-immich-checksum` header so Immich can
     short-circuit duplicates server-side.
3. Sits on libgphoto2's `wait_event` for the remainder of the session and
   processes `NewFile` events as you shoot.
4. JPEG+RAF pairs (same basename, both seen during the session — fresh or
   pre-existing) get stacked via `/api/stacks`. The JPEG is the primary.

## Requirements

- Linux desktop (D-Bus session for the optional notification popup).
- `libgphoto2` (Arch: `pacman -S libgphoto2`). On Debian/Ubuntu install
  `libgphoto2-6` plus `libgphoto2-dev` if building from source.
- Rust toolchain (stable) if you're building from source.

## Configuration (env vars)

| Var               | Required | Default | Notes                                              |
| ----------------- | -------- | ------- | -------------------------------------------------- |
| `IMMICH_URL`      | yes      | —       | e.g. `https://immich.example.com/`                 |
| `IMMICH_API_KEY`  | yes      | —       | `x-api-key` header value                           |
| `FUJI_TZ`         | yes      | —       | IANA TZ the camera's clock is set to (e.g. `Europe/Amsterdam`). libgphoto2 reports mtime as camera-local wall-clock reinterpreted as Unix epoch, so we need this to get true UTC. |
| `STACK_JPEG_RAF`  | no       | `true`  | Stack JPEG+RAF pairs via Immich's stacks API.      |
| `LOG_LEVEL`       | no       | `info`  | `tracing_subscriber` env filter (`debug`, `trace`…). |

## Running

```sh
IMMICH_URL=https://immich.example.com/ \
IMMICH_API_KEY=… \
FUJI_TZ=Europe/Amsterdam \
cargo run --release
```

The daemon prints what it's doing to stderr. When a camera comes up you
should see a `camera detected model=...` line; after the initial walk it
enters the event-driven mode where each shutter press triggers a NewFile
event and a sync.

## Camera-side caveats

### Fujifilm X-T3 (and similar bodies)

The X-T3 has to be in a PTP USB mode, not Card Reader. The menu path is

```
MENU/OK → SET UP → CONNECTION SETTING → PC CONNECTION MODE → "USB Tether Shooting Auto/Fixed"
```

In Card Reader mode the camera presents as USB Mass Storage and libgphoto2
ignores it. The CLI sanity check is `gphoto2 --auto-detect` — if that
doesn't see the camera, neither will fujimmich.

### gvfs auto-mount conflict (most desktop installs)

On GNOME / KDE / anything with `gvfs-gphoto2-volume-monitor` installed, the
desktop will autoclaim the camera the instant you plug it in, and
fujimmich will see `IoUsbClaim` errors. Easiest fix:

```sh
systemctl --user mask gvfs-gphoto2-volume-monitor.service
```

(or `pkill -f gvfs-gphoto2-volume-monitor` for a one-off.)

### udev permissions

The `libgphoto2` Arch package installs `/usr/lib/udev/rules.d/40-libgphoto2.rules`
that gives access to the `camera` ACL group. If `gphoto2 --auto-detect`
needs `sudo` to see your camera, you're not in the right group. On most
desktops you'll already be members via the seat/login session — only check
if it's broken.

## Running as a service

The repo ships a systemd **user** unit. Install + enable:

```sh
mkdir -p ~/.config/fujimmich ~/.config/systemd/user
cp packaging/systemd/env.example ~/.config/fujimmich/env
chmod 600 ~/.config/fujimmich/env
$EDITOR ~/.config/fujimmich/env   # fill in IMMICH_URL, IMMICH_API_KEY, FUJI_TZ
cp packaging/systemd/fujimmich.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now fujimmich.service
journalctl --user -u fujimmich -f
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

That puts the binary at `/usr/bin/fujimmich` and the systemd user unit at
`/usr/lib/systemd/user/fujimmich.service`. Per-user setup is still the env
file at `~/.config/fujimmich/env`.

## Notifications

Two notifications per session: one when the camera is detected ("Syncing
&lt;model&gt; → Immich"), one at session end with the count of newly-uploaded
assets (suppressed if zero). Nothing per file — the log is the source of
truth for per-file detail. Notifications are best-effort: if D-Bus isn't
available, the daemon logs the failure at debug level and keeps running.

## Development

```sh
cargo test          # 34 unit tests, mostly Immich client + stack tracker
cargo run           # against your real camera; needs the env vars above
```

There's no integration test against a real camera (or vcam — there isn't
a libgphoto2 equivalent that's easy to wire up). The Immich client is
covered by [wiremock][] tests; the rest needs hardware.

[wiremock]: https://crates.io/crates/wiremock

## What it intentionally doesn't do

- Network/Wi-Fi sync. Tried; X-T3 needs proprietary BLE wake which isn't
  worth implementing for v1.
- Multi-camera support. Single body, single instance.
- Local SQLite / state cache. Dedup goes to Immich every time.
- Push notifications when the camera is absent. The daemon just waits.
- Delete from the camera after upload. Photos stay on the card.

## License

TBD.
