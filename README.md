# irgate — IR Camera Face Unlock for Linux

> [!NOTE]
> **irgate** is derived from [pfalkingham/authFace](https://github.com/pfalkingham/authFace)
> (MIT), taken at commit `96a2fda`, including the improvements merged there from
> [SamVivan1/authFace](https://github.com/SamVivan1/authFace). The original
> README follows; it will be rewritten as irgate diverges. See [LICENSE](LICENSE).


By [Peter Falkingham](https://peterfalkingham.com)

<p align="center">
  <img src="data/irgate.svg" width="128" height="128" alt="irgate logo">
</p>

**Windows Hello–style biometric login for Linux.** IR camera facial authentication via PAM — works on **immutable distros** (Bazzite, Bluefin, Fedora Silverblue, Fedora Kinoite, etc.) with zero system packages, daemons, or layering.

> [!NOTE]
> Includes improvements contributed via [SamVivan1/authFace](https://github.com/SamVivan1/authFace) — robust IR camera detection, distro-aware PAM configuration, and the GNOME Shell lock-screen scan indicator. See [Upstream Merges & Security Pass](#upstream-merges--security-pass).

- **Face unlock for sudo, lock screen (GNOME/Sway), and `gdm-password`**
- **~2 seconds** from camera poll to authenticated
- **Static musl binary** — no dependencies, no runtime
- **No daemon, no systemd units, no D-Bus**
- **Immutable-first** — everything fits in `/usr/local` and `~/.local`, no `/usr` modifications needed

## Upstream Merges & Security Pass

Improvements merged from [SamVivan1/authFace](https://github.com/SamVivan1/authFace):

| Change | Before | Now |
|--------|--------|-----|
| **Multi-IR-camera support** | Returned the *first* IR-named device in `/sys/class/video4linux` | Collects all IR candidates and uses the first that **actually opens** as a GREY capture device |
| **Distro-aware PAM** | Only Fedora (`pam_selinux_permit.so` insertion point) | Also handles Ubuntu/Debian `gdm-password` (`#%PAM-1.0`) |
| **PAM `quiet` flag** | no `quiet` | `pam_exec.so quiet` suppresses `pam_exec` chatter |
| **Detector model download** | Required `models/version-slim-320.onnx` to be present | `deploy.sh` fetches it (since replaced by SCRFD, see [Model](#model)) |
| **Lock screen scan indicator** | None (silent scan) | `irgate` writes a status file; a GNOME Shell extension renders scanning/ok/fail |

Followed by a security pass over the whole tree — see [CHANGELOG.md](CHANGELOG.md)
for the full list. The changes that affect how you use it:

- **Enrolment needs root** (`sudo irgate-enroll`). Face templates are authentication data; when the store was
  world-writable, any local user could enrol a face for an account that had not
  enrolled yet, then log in as it.
- **The login prompt trusts only `/etc/irgate.toml`.** Your own config can
  make matching stricter, never looser. See [Configuration](#configuration).
- **`irgate` requires `PAM_USER`** rather than falling back to `USER`,
  `LOGNAME` or `id -un`, and refuses remote (`PAM_RHOST`) sessions.

Upgrading re-secures an existing template store in place, so **no re-enrolment
is needed**.

## Features

- **Windows Hello–compatible IR camera support** — raw GREY format, no RGB camera needed
- **Automatic password fallback** — if face auth fails, times out, or no camera, PAM falls through to password
- **Static musl binary** (~20 MB, zero runtime dependencies) — copy to any Linux system
- **No daemon, no systemd, no D-Bus** — just `pam_exec.so` triggered by PAM
- **Configurable** via `/etc/irgate.toml`, `~/.config/irgate.toml`, or environment variables
- **Built-in capture timeout** (5s default) — camera hang won't lock you out
- **Works on immutable distros** — no `rpm-ostree layer`, no package installs, no `/usr` modification

## Quick Start

```bash
# 1. Install core authentication (PAM, models, binaries)
sudo ./deploy.sh

# 2. Enroll your face (templates are root-owned, so this needs sudo)
sudo irgate-enroll --user $USER

# 3. Test sudo
sudo -k && sudo true   # triggers IR camera → exit 0
```

## Requirements

### Hardware

- **IR camera** exposing raw GREY format (Windows Hello compatible, e.g. Shinetech ASUS FHD webcam)
- **Linux kernel** with `uvcvideo` (standard on all distros)

> **Multiple IR cameras?** The fork's auto-detection finds *all* IR devices and picks the first one that opens. If you have more than one, set `device` explicitly in config to pin a specific one (see [Configuration](#configuration)).

### Software (target system — where you deploy)

- PAM with `pam_exec.so` (standard on all distros)
- SELinux (Fedora/Bluefin/Silverblue) — deploy script installs policy automatically
- `policycoreutils` for SELinux policy compilation (installed by default on Fedora)

### Software (build system — where you compile)

You need a Rust toolchain with the `x86_64-unknown-linux-musl` target.

## Building from Source

### Static musl build (no runtime deps)

```bash
# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Add musl target
rustup target add x86_64-unknown-linux-musl

# Clone and build
git clone https://github.com/PJM178/irgate.git
cd irgate
cargo build --release --target x86_64-unknown-linux-musl -p irgate -p irgate-enroll

# Deploy
sudo ./deploy.sh
```

### Without installing a toolchain (container build)

If `deploy.sh` finds no toolchain it prints this command for you to run. It
does not run it for you: `deploy.sh` runs under `sudo`, and rootless podman
driven through `sudo -u` often fails on a missing `XDG_RUNTIME_DIR`.

```bash
podman run --rm -v "$PWD":/src:Z -w /src docker.io/library/rust:alpine \
  sh -c 'apk add --no-cache musl-dev && \
         cargo build --release --target x86_64-unknown-linux-musl \
           -p irgate -p irgate-enroll'
sudo ./deploy.sh
```

`rust:alpine` targets musl natively, so the result is the same static binary.
Run the container as your own user (not under `sudo`) so the files in `target/`
stay yours.

### On immutable distros via distrobox

```bash
# Create a Fedora development container
distrobox create --image registry.fedoraproject.org/fedora:latest --name irgate-dev
distrobox enter irgate-dev

# Inside the container, install build deps (once).
# Note: Fedora does not package a musl std for Rust, so the musl build needs
# rustup rather than the distro `rust` package.
sudo dnf install -y gcc musl-gcc
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
  --target x86_64-unknown-linux-musl
source "$HOME/.cargo/env"

# Build
cd ~/Projects/irgate
cargo build --release --target x86_64-unknown-linux-musl -p irgate -p irgate-enroll

# Exit container, then deploy on host
exit
sudo ./deploy.sh
```

> **`sudo ./deploy.sh` and `cargo`:** if you installed Rust with rustup, `cargo`
> lives in `~/.cargo/bin`, which is not on root's `PATH`. The script looks there
> for the invoking user and runs the build as that user rather than as root, so
> `sudo ./deploy.sh` works and does not leave root-owned files in `target/`.

## Deployment

```bash
sudo ./deploy.sh
```

| Step | What | Details |
|------|------|---------|
| Build | Compiles if `cargo` is available | Falls back to pre-built binaries in `target/` |
| Binaries | Installs to `/usr/local/bin` | `irgate` + `irgate-enroll` |
| Models | Downloads InsightFace `buffalo_sc` if not staged in `models/` | `det_500m.onnx` + `w600k_mbf.onnx`, SHA-256 checked, to `/usr/local/share/irgate/` |
| Config | Installs default config | `/etc/irgate.toml` |
| PAM | Patches PAM service files | Adds `sufficient` `pam_exec.so quiet` to `sudo`, `gdm-password`, `swaylock` |
| SELinux | Compiles and loads policy | Allows `xdm_t` to mmap camera for lock-screen auth |
| Storage | Creates embeddings directory | `/var/lib/irgate/<user>/` with sticky bit |

GDM lock-screen patching is **distro-aware**:
- **Fedora/Bluefin/Silverblue** — inserts after `pam_selinux_permit.so`
- **Ubuntu/Debian** — inserts after `#%PAM-1.0`

Each PAM file is backed up with a `.irgate.bak` suffix.

### Upgrading from `face-auth` (before the rename)

Earlier builds of this project installed `face-auth` and `face-enroll`.
`sudo ./deploy.sh` moves an existing install over to the new names:

| Old | New |
|-----|-----|
| `/etc/face-auth.toml` | `/etc/irgate.toml` (model and template paths inside it are rewritten) |
| `/var/lib/face-auth/` | `/var/lib/irgate/`, so **no re-enrolment is needed** |
| `~/.config/face-auth.toml` (of the user running `sudo`) | `~/.config/irgate.toml` |
| `/usr/local/bin/face-auth`, `face-enroll` | removed; PAM now calls `/usr/local/bin/irgate` |
| `/usr/local/share/face-auth/`, SELinux module `face_auth` | removed; replaced by `irgate` equivalents |

`FACE_AUTH_*` environment variables are now `IRGATE_*`, and the journal tag is
`irgate` (`journalctl -t irgate`). If you installed the GNOME extension, run its
installer again and disable the old `authface-scan-indicator` one.

### Uninstall

```bash
# Remove binaries, models, config and PAM hooks
sudo ./uninstall.sh

# Remove everything including face embeddings
sudo ./uninstall.sh --purge
```

Restores PAM backups and removes binaries, models, config and the SELinux
policy. It also removes anything left by an install from before the rename
(`face-auth`, `face-enroll`, `/etc/face-auth.toml`, the `face_auth` SELinux
module) or by the old settings GUI.

## Configuration

The authentication path and the unprivileged tools trust different things.

**During PAM authentication** (`irgate`, i.e. sudo / lock screen / login):

| Source | Effect |
|--------|--------|
| `/etc/irgate.toml` (root-owned) | Authoritative for everything |
| `~/.config/irgate.toml` | May only make authentication **stricter** — see below |
| `IRGATE_*` environment | **Ignored entirely** |

**For `irgate-enroll`**, the usual layering applies:
environment variables, then `~/.config/irgate.toml`, then `/etc/irgate.toml`.

### What a user may override at the login prompt

A user's own config is read (resolved via `getent passwd`, so it is *their* home
and not whoever happened to invoke the PAM stack), but it is applied as a
narrowing overlay:

| Key | At the login prompt |
|-----|--------------------|
| `threshold`, `detector_threshold` | Honoured only if **>= the system value**. A lower number is ignored. |
| `device` | Honoured only if the path is a real IR capture device on this machine (IR-looking sysfs name, opens as GREY). |
| `scan_duration_ms`, `scan_interval_ms`, `capture_timeout_ms` | Honoured within built-in bounds. |
| `model_path`, `detector_model_path`, `embeddings_dir` | **Ignored** — system policy only. |

This is what stops code running as you — which does not know your password —
from writing a permissive `~/.config/irgate.toml` and turning your next
`sudo` into a root shell. To *loosen* matching, edit `/etc/irgate.toml` as
root.

Example `/etc/irgate.toml`:
```toml
device = "/dev/video2"   # usually best left unset; see below
threshold = 0.6
model_path = "/usr/local/share/irgate/w600k_mbf.onnx"
embeddings_dir = "/var/lib/irgate"
capture_timeout_ms = 5000
```

Environment variable names follow the field names, so the capture timeout is
`IRGATE_CAPTURE_TIMEOUT_MS` (not `IRGATE_CAPTURE_TIMEOUT`).

> **Leave `device` unset unless you must pin it.** UVC cameras normally expose
> a metadata node right beside the capture node under the same name — on the
> reference ASUS FHD webcam, `/dev/video2` captures and `/dev/video3` does not.
> Auto-detection opens each IR-named candidate and takes the first that is
> really a GREY capture device, which gets this right; a hand-written path
> often does not.

## Enrollment

Face templates live in a root-owned directory (`/var/lib/irgate`, mode
`0700`), so enrolment is a privileged operation:

```bash
# Replace existing embeddings with a new capture
sudo irgate-enroll --user $USER

# Append new embeddings to improve recognition across lighting/angles
sudo irgate-enroll --improve --user $USER
```

Enrolment captures 8 frames half a second apart. **Move while it runs**: turn and tilt
your head a little between frames. A frame whose embedding is near-identical to one
already captured is skipped, because eight copies of one pose teach the matcher
nothing about the other poses it will see at the lock screen.

CLI options: `--frames`, `--interval`, `--device`, `--threshold`, `--model`,
`--embeddings-dir`, `--improve`, `-v`.

To check the template without going through PAM:

```bash
sudo irgate --verify $USER   # exit 0 = match, 1 = no match, 2 = error
```

Why this is not user-writable: whatever can write a face template decides whose
face unlocks that account. If your own login could rewrite it, then so could
anything running as you, and a stolen browser session would become a root
shell at the next `sudo`.

## PAM Integration

The deploy script adds a `sufficient` `pam_exec.so quiet` line to:

| Service | File | Insertion point |
|---------|------|----------------|
| `sudo` | `/etc/pam.d/sudo` | After `#%PAM-1.0` |
| `gdm-password` | `/etc/pam.d/gdm-password` | After `pam_selinux_permit.so` (Fedora) / after `#%PAM-1.0` (Ubuntu/Debian) |
| `swaylock` | `/etc/pam.d/swaylock` | After `#%PAM-1.0` |

`sufficient` means: if irgate exits 0, the user is authenticated immediately.
If it fails (no match, no camera, timeout), PAM falls through to password prompt.

`quiet` suppresses PAM chatter on the lock screen so the unlock UI stays clean.

No `timeout`, `setenv`, or `env_pass` flags are needed — irgate reads the camera
(not stdin) and resolves `PAM_USER` via its own fallback chain.

## Lock Screen Scan Indicator (GNOME Shell extension)

By default the scan is silent: `irgate` runs headless inside PAM, so the only
feedback is the camera LED. A companion GNOME Shell extension shows live status
**on the lock screen** while the face is being scanned:

| Status | Indicator |
|--------|-----------|
| Scanning | Pulsing pill with camera icon + "Scanning face…" |
| Success | Green check — "Face recognised" (briefly) |
| Failure | Red error — "Face not recognised — use your password" |

### How it works

1. `irgate` (the PAM binary) writes a status file to the authenticated user's
   runtime directory while it runs: `/run/user/<uid>/irgate-status` containing
   `scanning`, then `ok` or `fail`.
2. The extension watches that file with a `Gio.FileMonitor` while the unlock
   UI is on screen, and renders the indicator above it.

   The file is written with `O_NOFOLLOW` because `irgate` runs as root and
   the runtime directory belongs to the user — otherwise a symlink there would
   aim a root write at any file on the system.

No daemon, no D-Bus server — just a small status file, keeping the zero-footprint
design of the core.

### Install

```bash
# Requires GNOME Shell 45+ (Fedora 39+, Bazzite, Bluefin, Silverblue, Kinoite)
extensions/irgate-scan-indicator/install-extension.sh
# Then: Alt+F2 → r (X11) or log out/in (Wayland)
```

Everything lives in `~/.local/share/gnome-shell/extensions/` — immutable-friendly.

> **Note:** this shows on the **session lock screen** (Super+L / auto-lock), not
> on the GDM login/greeter screen. The greeter runs in a separate locked-down
> shell as the `gdm` user and does not expose hooks for third-party indicators.

## How It Works

```
PAM (sudo / gdm-password / swaylock)
  │
  ▼
irgate (static binary)
  ├─ Resolve PAM_USER via getent (refuses to guess from USER/LOGNAME)
  ├─ Refuse if PAM_RHOST names a remote host
  ├─ Load /etc/irgate.toml + strictly-narrowing user overlay
  ├─ V4L2 capture from IR camera (640×400 GREY, auto-detected /dev/videoN)
  │   └─ poll() with 5s timeout — exits cleanly if camera hangs
  ├─ Frame quality gate (rejects unlit strobe frames and covered lenses)
  ├─ CLAHE contrast equalisation
  ├─ Face detection with 5 landmarks (SCRFD det_500m, 320×320 letterbox)
  ├─ Similarity-transform warp of the face to the 112×112 ArcFace layout
  ├─ tract-onnx inference (MobileFaceNet w600k_mbf, 512-d embedding)
  ├─ Cosine similarity vs stored templates (default threshold 0.5)
  ├─ One journal line per scan: outcome, best score, frames, reject reason
  └─ Exit 0 (match) or exit 1 (no match → password prompt)
```

## Model

Both models come from InsightFace's `buffalo_sc` pack. InsightFace's code is MIT, but
its pretrained models are licensed for **non-commercial research use only**; check
[InsightFace's licence terms](https://github.com/deepinsight/insightface#license) before
using irgate anywhere that is not personal or research use. irgate's own licence does
not cover the models.

| File | Role | Size |
|------|------|------|
| `det_500m.onnx` | SCRFD face detector with five landmarks | 2.5 MB |
| `w600k_mbf.onnx` | MobileFaceNet @ WebFace600K recogniser, 512-d output | 13 MB |

Neither is bundled in this repository. `deploy.sh` downloads the pack once from
InsightFace's official GitHub release, verifies each file's SHA-256, and installs both
to `/usr/local/share/irgate/`. A copy staged in `models/` is used instead of
downloading, and is checked the same way.

### Why alignment matters

The recogniser was trained on 112×112 crops in which the eyes, nose and mouth sit at
fixed positions. authFace fed it the whole 640×400 frame squashed to 112×112, so the
embedding mostly described the room. On the reference machine, with the detector's
check bypassed, frames of the **empty chair scored 0.65–0.77** against the enrolled
template, above the 0.6 threshold. The empty chair would not unlock on its own (the
detector still has to see a face), but the detector's yes/no "is any face present" was
the only face-specific check. How a different person in the same seat would score was
not measured. irgate detects the face, warps it
onto the ArcFace landmark layout, and encodes only that. With nobody in view there are
no landmarks and nothing is scored; with the enrolled user it scores 0.60–0.95 across
posture changes (mean 0.84) where the old pipeline gave 0.66–0.81.

**Templates made by authFace are refused** (format version 1; irgate writes version 2)
with a message telling the user to re-enrol. Password fallback applies until they do.

## SELinux

On Fedora/Bluefin/Silverblue with SELinux enforcing, the GNOME lock screen runs in the
`xdm_t` domain. This domain cannot `mmap` video devices by default. The deploy script
installs a minimal policy module:

```
allow xdm_t v4l_device_t:chr_file map;
```

To remove: `sudo semodule -r irgate`

If the deploy script reported missing SELinux tools:
```bash
sudo dnf install -y policycoreutils
sudo checkmodule -M -m -o irgate.mod selinux/irgate.te
sudo semodule_package -o irgate.pp -m irgate.mod
sudo semodule -i irgate.pp
```

## Troubleshooting

```bash
# List available IR cameras
ls /sys/class/video4linux/*/name

# Grant video group access (log out/in after)
sudo usermod -aG video $USER

# Debug output from a live sudo attempt
sudo -k; RUST_LOG=irgate_core=debug,irgate=debug sudo true

# Every scan leaves one line here: match / no match, best similarity vs
# threshold, frames with a face, and why frames were rejected if none had one
journalctl -t irgate

# pam_exec's own view
journalctl | grep -i "pam_exec\|irgate"

# SELinux denials
journalctl -k | grep irgate | grep denied

# Test a stored face directly (skips PAM; needs root to read templates)
sudo irgate --verify $USER
echo $?   # 0 = match, 1 = no match, 2 = error

# Raise the capture timeout (note the _MS suffix)
IRGATE_CAPTURE_TIMEOUT_MS=10000 sudo irgate --verify $USER
```

### "PAM_USER is not set"

`irgate` no longer guesses the account from `USER`/`LOGNAME`. If you are
invoking it by hand, use `--verify` rather than setting `PAM_USER` yourself.

### "reports pixel format ... requires raw 8-bit GREY"

The configured device is not an IR sensor — it is an ordinary RGB webcam, or the
metadata node that sits next to the real capture node. Unset `device` to let
auto-detection pick one, or check `v4l2-ctl --device /dev/videoN --list-formats`.

### "no IR camera found"

No `/dev/video*` node opened as a raw GREY capture device. Run
`cargo run --example detect-camera` to see each node and why it was rejected.
The sysfs name is informational only: the kernel truncates it to 31 characters,
so an IR sensor may be listed as "... USB2.0 I" with the "R" cut off.

### "no face detected" every time

Most Windows Hello IR modules **strobe their illuminator**, emitting a lit frame
and a near-black one alternately. Check what yours does:

```bash
cargo run --example frame-stats
```

On the reference ASUS sensor the lit frames mean 71–237 (of 255) and the unlit
ones 2–5, strictly alternating at 15 fps. irgate captures frames in pairs and
keeps the brighter, so this is handled — but if `frame-stats` shows *every*
frame dark, the illuminator is not firing and no amount of software will help.

A dark frame is worse than useless: histogram equalisation stretches its narrow
range across the full scale and turns sensor noise into a high-contrast grey
field in which no face can be found.

### Which camera will it use?

```bash
cargo run --example detect-camera
```

Lists every V4L2 node, whether its name looks like an IR sensor, whether it
really opens as a GREY capture device, and which one authentication would pick.

### Multi-camera picks the wrong device / no device selected

```bash
# See which IR device is detected
sudo irgate --verify $USER   # with RUST_LOG=irgate_core=debug

# Pin a specific camera in your own config (must be a real IR device)
echo 'device = "/dev/video2"' >> ~/.config/irgate.toml
```

### My threshold change did nothing

A user config may only make matching *stricter*. To loosen it, lower
`threshold` in `/etc/irgate.toml` as root — see [Configuration](#configuration).

## Security & Limitations

### Trust model

- **Face templates are root-owned.** `/var/lib/irgate` is mode `0700`,
  root:root, with templates at `0600`. Whatever can write a template decides
  whose face unlocks that account, so enrolment goes through `sudo`.
- **The PAM path trusts only `/etc/irgate.toml`.** A user's own config may
  make matching stricter, never looser, and may not redirect the model or
  template paths. `IRGATE_*` environment variables are ignored during
  authentication. See [Configuration](#configuration).
- **Identity comes from `PAM_USER` only.** `irgate` refuses to run if PAM
  did not set it, rather than falling back to `USER`, `LOGNAME` or `id -un`.
- **Remote sessions are refused.** If `PAM_RHOST` names a non-local host,
  face authentication is declined — the camera is at the console, so otherwise
  whoever is sitting at the desk would authenticate an SSH session.

### Known limitations

- **IR-only, no liveness detection:** an IR camera resists casual photo
  spoofing, but there is no structured-light or dot-projection depth check.
  A high-quality IR-visible print or a 3D mask may bypass verification. This is
  the main residual risk and it is inherent to the approach — treat face unlock
  as a convenience over a password you still have, not as a stronger factor.
- **No rate limiting or lockout.** Every prompt allows a fresh scan window.
  PAM's own `pam_faildelay`/`pam_tally2` are not wired up.
- **`sufficient` bypasses the rest of the auth stack.** A successful match
  satisfies authentication outright; any other `auth` module below the
  irgate line is skipped. That is the point, but it means the strength of
  the whole stack becomes the strength of the face match.
- **SELinux policy scope:** the lock-screen policy grants `xdm_t` mmap access
  to all V4L2 devices. A trade-off for drop-in compatibility; narrowing it
  requires custom udev device types.
- **x86_64 only:** V4L2 ioctl numbers and struct layouts are hardcoded.
  ARM/aarch64 requires switching to the `v4l` crate.
- **Model integrity:** both ONNX models are pinned by SHA-256 and the detector
  URL is pinned to a commit, not a branch. `deploy.sh` aborts on mismatch.

## Project Structure

```
irgate/
  crates/
    irgate-core/             # Core library
      src/
        capture.rs           # V4L2 capture + poll() timeout + IR camera auto-detect
        config.rs            # Layered config + narrowing overlay for PAM + per-user load
        detector.rs          # SCRFD face detection with landmarks + frame quality gate
        error.rs             # Error types
        inference.rs         # tract-onnx model loading + encoding
        lib.rs               # FaceAuth struct, auth + enroll + scan
        preprocess.rs        # CLAHE, similarity transform, landmark-aligned face warp
        storage.rs           # Binary embedding I/O (versioned, atomic, 0600)
        user.rs              # NSS lookup + username validation
        verify.rs            # Cosine similarity
    irgate/                  # PAM binary (stdin-less, PAM_USER fallback)
    irgate-enroll/           # Enrollment CLI
  config/
    irgate.toml.example      # Documented config template
  data/
    *.svg                    # Project icon (used in this README)
  selinux/
    irgate.te                # SELinux policy source
  extensions/
    irgate-scan-indicator/   # GNOME Shell lock-screen scan indicator
      extension.js
      metadata.json
      install-extension.sh
  deploy.sh                  # Core auth installer
  uninstall.sh               # Removal script (--purge also deletes templates)
```

## License

MIT

This is a fork of [pfalkingham/authFace](https://github.com/pfalkingham/authFace) (MIT).
The face models (`w600k_mbf.onnx`, `det_500m.onnx`) are not part of this repository and
are not MIT: `deploy.sh` downloads them from InsightFace, whose pretrained models are for
non-commercial research use only. See [Model](#model).
