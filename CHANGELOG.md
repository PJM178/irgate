# Changelog

## irgate (formerly irauth)

Changes since the fork from authFace at `96a2fda`.

### Renamed

- **The project is now irgate.** "irauth" was already in use by an unrelated
  Linux IR face-authentication project. Everything that carried the old
  `face-auth` name follows:

  | Before | Now |
  |--------|-----|
  | `face-auth`, `face-enroll` | `irgate`, `irgate-enroll` |
  | `face-auth-core` crate | `irgate-core` |
  | `/etc/face-auth.toml`, `~/.config/face-auth.toml` | `/etc/irgate.toml`, `~/.config/irgate.toml` |
  | `/var/lib/face-auth`, `/usr/local/share/face-auth` | `/var/lib/irgate`, `/usr/local/share/irgate` |
  | `FACE_AUTH_*` environment variables | `IRGATE_*` |
  | journal tag `face-auth` | `irgate` |
  | SELinux module `face_auth` | `irgate` |
  | `/run/user/<uid>/face-auth-status` | `/run/user/<uid>/irgate-status` |
  | `authface-scan-indicator@samvivan.local` | `irgate-scan-indicator@pjm178.github.io` |

  `deploy.sh` migrates an existing install: it moves the config (rewriting the
  model and template paths in it) and the template store, so nobody has to
  re-enrol, then removes the old binaries, models, PAM lines and SELinux
  module. `uninstall.sh` removes both old and new names.
- `deploy.sh` only warns about templates that need re-enrolment when one is
  actually format version 1; it used to warn whenever any template existed.
- README: the licence section called the InsightFace models MIT, contradicting
  the Model section. They are non-commercial research only.

### Added

- **Scan outcomes are recorded in the system journal.** Every run of
  `face-auth` now logs one line via syslog(3) under the `face-auth` identifier
  (facility AUTHPRIV): match or no match, the best similarity seen against the
  threshold, how many frames carried a face, and why frames were rejected if
  none did. Previously a failed unlock left nothing but `pam_exec`'s exit code,
  so a covered lens, a mis-set threshold and a camera fault were
  indistinguishable. See it with `journalctl -t face-auth`.
- `FaceAuth::authenticate_scan` returns a `ScanOutcome` instead of a `bool`,
  and `verify::best_similarity` exposes the score itself.

### Security

- **The recogniser was matching the room, not the face.** The detector's
  bounding boxes were discarded and the encoder was fed the whole 640x400
  frame squashed to 112x112, so the embedding was dominated by the
  background. Measured on the reference machine with the detector's check
  bypassed, frames of the empty chair scored 0.65-0.77 against the enrolled
  template, above the 0.60 threshold. An empty chair does not unlock on its
  own, because the detector still has to report a face, but that yes/no
  "is any face present" was the only face-specific check in the login path.
  Whether a different person in the same seat would match was not measured;
  the scores suggest it is likely. The pipeline now detects the face with
  InsightFace's SCRFD (`det_500m.onnx`, from the same `buffalo_sc` pack as
  the recogniser, which `deploy.sh` had been downloading and discarding),
  warps it onto the ArcFace five-landmark layout, and encodes only that. With
  nobody in view there are no landmarks and nothing is scored. Same-user
  similarity across posture changes rose to 0.60-0.95 (mean 0.84) from
  0.66-0.81, and detection got faster (10 ms vs 34 ms per frame).
- **Templates carry a format version and old ones are refused.** The
  template file is now version 2; a version-1 file from authFace produces a
  clear "re-enrol" error (and password fallback) rather than being compared
  against incomparable vectors.

### Changed

- **Default threshold is 0.5** for new installs (an existing
  `/etc/face-auth.toml` is kept). Measured same-user scores span 0.60-0.95
  across poses; a moving, partly out-of-frame face scored 0.49. Impostor
  scores were not measured and should be checked with a second person before
  lowering it further.
- **Enrolment captures 8 frames, 500 ms apart, and skips near-duplicates**
  (similarity >= 0.97 to a frame already captured in the same run), so the
  user is nudged to move their head and the templates cover the poses the
  lock screen will see.
- **tract upgraded to 0.23.** tract 0.21 silently computed garbage for the
  SCRFD graph (box values around 1e25, unoptimised graph failing with a
  broadcast error at an Add node); 0.23 runs it correctly. The recogniser
  also got faster: 22 ms vs 38 ms per frame.
- `cargo run --example probe` enrols into a directory you own and scores
  live frames against it, including an empty-room mode; this is how the
  numbers above were measured.

### Removed

- **The GTK4 settings GUI (`face-auth-gtk`, `deploy-gui.sh`).** It had not
  been built or run since the fork, had already fallen behind the command-line
  tools (it enrolled 5 frames instead of 8), and everything it did is one
  command: `sudo face-enroll --user $USER [--improve]`,
  `sudo face-auth --verify $USER`, and editing `/etc/face-auth.toml`. Dropping
  it also drops the GTK4/libadwaita build dependency and the `pkexec` path into
  the root-only tools. `uninstall.sh` loses its `--gui` flag but still removes
  files an earlier GUI install left behind. `user::invoking()`, which only the
  GUI used, is gone with it. The code remains in git history.

### Fixed

- **Face unlock now works in a dark room.** In darkness (no ambient IR, so
  the unlit strobe frame is fully black) the reference ASUS sensor
  (3277:0060) streams about 12 good frames and then replaces every frame with
  a solid fill of 144. That's firmware behaviour: Windows' inbox driver sends
  nothing that prevents it, and Windows Hello fails in the dark on the same
  hardware (checked in a Windows 11 VM with the camera passed through and the
  USB traffic recorded). Three changes make the dark usable:
  - `Camera::capture_frame` detects a flat-fill frame and restarts the stream,
    which gives another ~0.8 s of real frames (restart cost about 250 ms).
  - Auto-exposure is metered on the face through the standard UVC 1.5 region
    of interest control, as Windows Hello does: the frame centre at first,
    then each detected face's box. Metered on the whole dark room, the sensor
    blew the face out (lit-frame means 100-230, detector scores under 0.4);
    on the face it holds about 35-55 from the first frame.
  - Enrollment (and a non-zero `scan_interval_ms`) pauses the stream while
    sleeping. A stream left running but unread blanks and lets exposure drift
    to maximum before the next capture.

  Measured in the dark: enrollment 8/8 templates with no rejected attempts
  (it saved 1-4 before), and a 10 s check matched 61/61 frames (similarity
  mean 0.88, min 0.67). New diagnostics: `darkstream`, `pacing`, `detectraw`.

- **Camera auto-detection found nothing on the reference laptop and fell
  back to the RGB webcam.** It required the sysfs card name to contain the
  word "IR", but the kernel truncates card names to 31 characters and the
  ASUS sensor reports "USB2.0 FHD UVC WebCam: USB2.0 I". Detection now takes
  any `/dev/video*` node that opens as a raw GREY capture device (what IR
  sensors expose and RGB webcams do not), preferring IR-named ones. When no
  camera is found, `face-auth` and `face-enroll` now say so and point at
  `device` in `/etc/face-auth.toml` instead of silently trying `/dev/video0`
  and failing with "reports pixel format 'MJPG'". The same rule applies to a
  user's `device` override. `cargo run --example detect-camera` lists every
  node with the reason it was or was not accepted.
- **Face detector input was normalised to 0..1 instead of the model's
  (pixel - 127) / 128.** It still found faces, but on the reference camera the
  same frames scored 0.74-0.96 instead of 1.00, halving the margin above the
  0.5 detection threshold for nothing.

### Changed

- Release builds are stripped and built with fat LTO and a single codegen
  unit. The static `face-auth` binary drops from 26.7 MB to 18.2 MB and
  per-frame inference by about a third.

## [Unreleased]

Merges the improvements from the `SamVivan1/authFace` fork and follows them
with a security pass over the whole tree.

### Security

Three issues combined into a local privilege escalation on a deployed system.

- **Face templates are no longer writable by unprivileged users.**
  `deploy.sh` set `/var/lib/face-auth` to mode `1777` with user-owned
  per-account subdirectories. The sticky bit prevents deleting other people's
  entries but not creating new ones, so any local user could create a template
  directory for an account that had not enrolled yet — including `root` — and
  then authenticate as it at the login screen. Equally, because each user owned
  their own template, code running as them could substitute an attacker's face
  and escalate through their next `sudo`. The store is now root-owned `0700`
  with templates at `0600`; `deploy.sh` re-secures an existing store in place,
  so nobody has to enrol again.
- **The PAM path no longer trusts user-writable configuration.**
  `FaceAuthConfig::load_for_auth` reads only `/etc/face-auth.toml`, then applies
  the user's own config as a strictly narrowing overlay: `threshold` and
  `detector_threshold` are honoured only if at least as strict as the system
  value, `device` only if it is a real IR capture device on this machine, and
  `model_path` / `detector_model_path` / `embeddings_dir` not at all. The
  environment is ignored entirely. Previously a `~/.config/face-auth.toml` with
  `threshold = 0.1` and a redirected `embeddings_dir` turned the next `sudo`
  into root.
- **Identity comes from `PAM_USER` alone.** The `PAM_USER` → `USER` →
  `LOGNAME` → `id -un` fallback chain let environment strings decide which
  template was checked, and under `sudo` the `id -un` branch resolved to
  `root`. Usernames are now resolved through NSS and validated before becoming
  a path component, closing a path traversal into an arbitrary embeddings file.
- **Fixed an arbitrary file write as root** in the fork's new scan-indicator
  support. `write_status` wrote `/run/user/<uid>/face-auth-status` with
  `fs::write` while running as root; that directory belongs to the user, so
  symlinking the status file at, say, `/etc/shadow` had root truncate it on the
  next unlock. The write now uses `O_NOFOLLOW`.
- **Refuse face authentication for remote sessions.** If `PAM_RHOST` names a
  non-local host, `face-auth` declines rather than polling a camera that is
  physically next to someone else.
- **Bounded the embedding file.** `count` was read as a `u32` straight from
  disk and passed to `Vec::with_capacity`, so a crafted file requested ~96 GB.
  Loads now reject counts above 256, non-finite values, and trailing bytes.
- **Clamped `bytesused` against the mmap length** in `capture_frame`, the one
  place external data sizes an `unsafe` slice; an oversized value from a buggy
  or hostile driver read past the end of the buffer.
- **Models staged in `models/` are now checksum-verified too.** Only the
  download path checked, so a file placed there by hand — which the script
  prefers over downloading, and which the docs now suggest as a fallback —
  was installed unverified.
- **Verified the detector model.** `deploy.sh` downloaded it over `curl -sL`
  with no `-f`, no exit check and no checksum, so an error page could be
  installed as the model. The URL is now pinned to a commit rather than
  `master`, and the SHA-256 is checked.
- **Removed a `/tmp` staging race** in `deploy.sh`: the fixed
  `/tmp/face-auth-model` path could be pre-created and owned by another user,
  who could then swap the model between checksum verification and install.
  Staging now uses `mktemp -d`.

### Added

- `face-auth --verify USER` — verify a stored face without going through PAM
  (requires root). Used by the GUI's Test button.
- GUI Enroll / Improve / Test run through `pkexec`, so enrolment against a
  root-owned store stays a single click.
- `face_auth_core::user` module: NSS-backed lookup plus username validation.
- `cargo run --example detect-camera` — diagnostic listing every V4L2 node, why
  each is or is not treated as an IR sensor, and which one would be used.
- `cargo run --example frame-stats` — captures consecutive frames and prints
  per-frame mean, variance and min/max plus PNG dumps. This is what identified
  the strobing illuminator; keep it for diagnosing "no face detected".
- Tests covering the config trust boundary, username validation, embedding
  file parsing, verification edge cases, IR name matching and frame geometry.

### Fixed

- **The lock-screen indicator never appeared.** The extension tested
  `Main.sessionMode.currentMode === 'lock'`, but GNOME has no such mode — the
  shield is `lock-screen` and the unlock prompt is `unlock-dialog`. It also
  lacked `session-modes` in `metadata.json`, so it was disabled on the lock
  screen regardless. Both fixed, and the bubble is now actually centred rather
  than pinned to the left edge.
- **The documented device path was the wrong node.** `config` and the README
  named `/dev/video3` as the IR camera; on the reference ASUS FHD webcam that
  is the metadata node and `/dev/video2` is the capture node. Verified against
  the hardware: auto-detection now resolves `/dev/video2`, rejects `/dev/video3`
  (no capture format) and rejects `/dev/video0` (reports MJPG, not GREY).
- **The GUI preview flickered between a real IR image and a grey mess, and
  enrolment could never succeed.** The IR module strobes its illuminator,
  emitting a lit frame and a near-black one alternately at 15 fps (measured on
  the reference ASUS sensor: lit frames mean 48-96, dark 1.8-8). Two bugs
  compounded:
    - `raw_frame_has_content` required a variance above 100_000 in the u16
      domain, which is a variance of **1.5** in 8-bit units. Dark frames measure
      3.4-39, so they passed. `histogram_equalize` then stretched their 0-23
      range across the full scale, turning sensor noise into a high-contrast
      grey field that the detector searched in vain.
    - Enrolment's 400 ms interval is almost exactly 6 frames at 14.98 fps. Six
      is even, so it could lock onto the dark phase and see *only* unusable
      frames for every attempt.
  Frames are now captured in pairs, keeping the brighter — which needs no
  assumption that a strobe exists, since on a steady camera the two are alike.
  The quality gate checks mean brightness as well as variance, in documented
  8-bit units, and reports which check failed rather than a bare "no face".
- **Face detection failed on nearly every frame, because global histogram
  equalisation was destroying the image.** A dark IR frame has almost all its
  samples in a narrow band; mapping one CDF over the whole frame stretched that
  band across the full range and turned sensor noise into hard posterised
  contours. Measured on the reference camera with the detector's 0.5 threshold:

  | preprocessing | detector score |
  |---------------|----------------|
  | raw | 0.11 - 0.22 |
  | global equalisation (old) | **0.11 - 0.13** |
  | CLAHE clip 2.0 | 0.60 - 0.96 |
  | CLAHE clip 3.0 (new default) | **0.76 - 0.98** |

  Live burst before: 0/24 frames detected. After: 16/16 at ~0.99.

  Replaced with contrast-limited adaptive histogram equalisation — equalise per
  tile with a ceiling on amplification, bilinearly interpolated between tiles.
  This is what Howdy does (`cv2.createCLAHE`), and comparing against Howdy is
  what identified the cause.

  **This changes what an embedding means, so existing enrolments must be
  redone.** Since enrolment was failing anyway, nothing is lost.
- **The GUI enrolled the wrong account when started with `sudo`.** It picked
  its target user from `getuid()`, so `sudo face-auth-gtk` ran as root and its
  Enroll, Improve and Test buttons all operated on **root's** template. All
  three agreed with each other and appeared to work, while PAM authenticated
  the desktop user against a template that was never written — "works in the
  GUI, fails at the lock screen". The GUI now resolves the invoking user via
  `SUDO_UID`/`PKEXEC_UID` before falling back to the process UID, and shows an
  **Account** row naming exactly whose face the buttons apply to, with a
  warning when that resolves to root.
- **`"ir"` was matched as a substring** when detecting IR cameras, so
  "Virtual Camera" (v4l2loopback) and "Logitech BRIO" both registered as IR
  sensors. Matching is now on word boundaries.
- **The capture pipeline assumed 8-bit GREY without checking.** Pointed at an
  RGB webcam it reinterpreted YUYV or MJPEG bytes as greyscale and compared the
  noise against a real template. `Camera::open` now validates the pixel format
  and frame geometry and explains the mismatch.
- `cosine_similarity` silently compared a prefix when the stored and probe
  embeddings differed in length; the seed of `0.0` also made anti-correlated
  matches indistinguishable from orthogonal ones.
- `detect()` panicked on an unexpected detector output shape instead of
  returning an error — inside PAM.
- `enroll_append` treated *any* load failure as "nothing enrolled yet" and
  would overwrite an existing, merely-unreadable template file.
- `face-enroll --user 0` passed `getent` validation and enrolled into a
  directory named `0` that authentication never reads. Names now resolve to
  their canonical form.
- A failed `VIDIOC_DQBUF` left no queued buffer, so every later poll in a scan
  window timed out.
- Frames shorter than `width * height` were zero-padded into a half-black image
  rather than rejected.
- `raw_frame_has_content` accumulated variance in `f32` over ~256k large terms,
  well past its precision; it now uses `f64`.
- Embedding writes are `fsync`ed before the rename, so a crash cannot leave a
  present-but-empty template file.
- GUI config writes are atomic and no longer clobber `detector_threshold` when
  saving `threshold`.
- `uninstall.sh` deleted any PAM line containing `face-auth`; it is now
  anchored to the `pam_exec.so` stanza.
- `deploy.sh` reports a warning instead of silent success when it cannot find
  a PAM insertion point, and refuses to run as non-root.
- **Model downloads retry.** GitHub release downloads redirect to a CDN that
  intermittently resets the connection mid-handshake ("TLS connect error:
  unexpected eof while reading"), which aborted the install halfway through.
  `curl` now retries with `--retry-all-errors`, and on persistent failure the
  script prints the exact commands to stage the file in `models/` by hand.
- **`deploy.sh` could not build on a machine without a toolchain**, and its
  error pointed at a `face-auth-dev` distrobox that only existed on the
  original development machine. It now:
  - finds `cargo` in the invoking user's `~/.cargo/bin`, which root's `PATH`
    does not include, so `sudo ./deploy.sh` works after a rustup install;
  - runs the build as that user rather than as root, so cargo does not fetch
    crates and run build scripts as root or leave root-owned files in `target/`;
  - offers to build in a `rust:alpine` container when no toolchain is present;
  - gives distro-specific install commands instead of naming a container that
    may not exist;
  - verifies the expected binaries exist before trying to install them.
  `deploy-gui.sh` gets the same cargo discovery and non-root build (no container
  fallback — the GUI links against the host's GTK4).
- **All three scripts aborted with "USER: unbound variable"** under `set -u`
  wherever the environment does not define `USER`/`HOME` (containers, cron,
  some sudo configurations). They now fall back to the real uid via `id -un`
  and `getent`.
- `face-auth` exited silently on a misconfiguration, because setup failures and
  routine auth outcomes both logged below the default level. Setup failures
  (no `PAM_USER`, bad config, missing model, unusable camera) now log at
  `error` so they are visible without setting `RUST_LOG`; "no match" and
  declined remote sessions stay quiet, so `pam_exec` does not print on every
  failed sudo.

### Performance

- **Unlock is ~2.6x faster per scan attempt.** Neither ONNX model was ever run
  through `into_optimized()`, so tract executed both graphs op-by-op as written;
  the detector had no input fact either, leaving its shapes symbolic and
  optimisation impossible. Measured on the reference machine:

  | stage | before | after |
  |-------|--------|-------|
  | encode | 398 ms | 112 ms |
  | detect | 177 ms | 75 ms |
  | per attempt | 622 ms | 235 ms |

  Model loading rises from 25 ms to 313 ms, since the optimisation pass now runs
  at load. That is paid once per unlock and repaid within the first attempt.

  Verified numerically equivalent: encoding a fixed input before and after gives
  cosine similarity 1.0000000000, max element difference 2.4e-7. **Existing
  enrolled templates remain valid.**
- **`scan_interval_ms` now defaults to 0** (was 200). It was pure sleep between
  attempts. The loop cannot spin — the camera delivers 15 fps and one attempt
  costs ~235 ms of inference — so the delay bought nothing and cost most of a
  second across a handful of attempts.
- Added `cargo run --example bench`, which reports model-load cost and a
  per-stage breakdown of each scan attempt.

### Changed

- **Enrolment now requires root** (`sudo face-enroll`), a direct consequence of
  the template store no longer being world-writable. The GUI hides this behind
  `pkexec`.
- **The GUI threshold slider starts at the system value**, since a lower one
  would be ignored at the login prompt.
- The GUI preview holds the camera open instead of reopening it — full
  `open`/`G_FMT`/`REQBUFS`/`QUERYBUF`/`mmap`/`STREAMON` — for every frame, and
  no longer runs the 512-d encoder on each frame only to discard the result.
  Detection is paced at ~7 Hz while the preview streams at camera rate.
- GUI helper invocations run off the main loop, so the window no longer freezes
  for the several seconds an enrolment takes. The preview channel is bounded
  and drained, so it cannot grow without limit or drift behind.
- `TIMING`/`SCAN` output on every authentication now goes through `tracing` at
  debug level instead of `eprintln!` to stderr, where `pam_exec` put it on the
  terminal for every `sudo`.
- Config values are range-checked; a `threshold` at or below zero is rejected
  rather than silently accepting every face.
- Histogram equalisation tables moved off the stack (512 KB per call).
- GNOME extension strings are in English and the status file is watched with a
  `Gio.FileMonitor` rather than polled every 100 ms for the whole session.
- `README.md` documents the trust model, the narrowing overlay, and the
  residual risks (no liveness detection, no rate limiting).

### Merged from SamVivan1/authFace

- Open-probe IR camera detection — try each candidate and use the first that
  actually opens, rather than the first name that matches.
- Distro-aware `gdm-password` patching for Ubuntu/Debian.
- `quiet` on the `pam_exec` stanzas.
- GNOME Shell lock-screen scan indicator.

## [Earlier]

### Fixed
- Removed `timeout=10` from PAM stanzas (causes pam_exec to block on stdin; face-auth reads the camera, not stdin)
- `deploy.sh` no longer wipes `/var/lib/face-auth/` on redeploy (preserves enrolled users)
- Model checksum mismatch now aborts deployment instead of continuing with potentially corrupted model
- User config (`~/.config/face-auth.toml`) now correctly overrides system config (`/etc/face-auth.toml`)
- `face-enroll` validates that the target user exists before attempting enrollment
- Camera buffer mmap changed to `PROT_READ` only (principle of least privilege)

### Added
- `uninstall.sh --purge` flag to optionally remove user embeddings
- Architecture and security limitation documentation in README
- CHANGELOG.md

### Performance

- **Unlock is ~2.6x faster per scan attempt.** Neither ONNX model was ever run
  through `into_optimized()`, so tract executed both graphs op-by-op as written;
  the detector had no input fact either, leaving its shapes symbolic and
  optimisation impossible. Measured on the reference machine:

  | stage | before | after |
  |-------|--------|-------|
  | encode | 398 ms | 112 ms |
  | detect | 177 ms | 75 ms |
  | per attempt | 622 ms | 235 ms |

  Model loading rises from 25 ms to 313 ms, since the optimisation pass now runs
  at load. That is paid once per unlock and repaid within the first attempt.

  Verified numerically equivalent: encoding a fixed input before and after gives
  cosine similarity 1.0000000000, max element difference 2.4e-7. **Existing
  enrolled templates remain valid.**
- **`scan_interval_ms` now defaults to 0** (was 200). It was pure sleep between
  attempts. The loop cannot spin — the camera delivers 15 fps and one attempt
  costs ~235 ms of inference — so the delay bought nothing and cost most of a
  second across a handful of attempts.
- Added `cargo run --example bench`, which reports model-load cost and a
  per-stage breakdown of each scan attempt.

### Changed
- Clarified SELinux policy scope and trade-offs in documentation
- Added x86_64-only architecture warning to V4L2 capture module
