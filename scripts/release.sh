#!/usr/bin/env bash
# Build the release tarball and checksums into dist/.
#
# The tarball is `git archive HEAD` plus the static musl binaries at
# target/x86_64-unknown-linux-musl/release/, where deploy.sh picks them up,
# so it installs without a Rust toolchain.
#
# Build paths are remapped so the binaries do not carry the builder's home
# directory (panic locations embed source paths), and the script refuses to
# package them if any trace of it remains.
set -euo pipefail

cd "$(dirname "$0")/.."
ROOT="$PWD"

if [ -n "$(git status --porcelain)" ]; then
    echo "Working tree is not clean; commit or stash first." >&2
    exit 1
fi

VERSION="$(cargo metadata --no-deps --format-version 1 \
    | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="irgate"))')"
TARGET="x86_64-unknown-linux-musl"
NAME="irgate-$VERSION-x86_64-linux"
DIST="$ROOT/dist"
CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"

# rustc applies the last matching remapping, so the catch-all $HOME goes first.
export RUSTFLAGS="${RUSTFLAGS:-} \
    --remap-path-prefix=$HOME=/build \
    --remap-path-prefix=$RUSTUP_HOME=/rustup \
    --remap-path-prefix=$CARGO_HOME/registry/src=/cargo/registry/src \
    --remap-path-prefix=$CARGO_HOME/git/checkouts=/cargo/git/checkouts \
    --remap-path-prefix=$ROOT=/irgate"
# A separate target dir: different RUSTFLAGS would otherwise invalidate the
# everyday build in target/. Fat LTO is memory hungry, so cap parallelism.
export CARGO_TARGET_DIR="$ROOT/target/dist"
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"

echo "Building irgate $VERSION for $TARGET..."
cargo build --locked --release --target "$TARGET" -p irgate -p irgate-enroll

BIN_DIR="$CARGO_TARGET_DIR/$TARGET/release"
leak=0
for bin in irgate irgate-enroll; do
    for needle in "$HOME" "$ROOT" "$(id -un)"; do
        if grep -qaF -- "$needle" "$BIN_DIR/$bin"; then
            echo "$bin still contains '$needle'" >&2
            leak=1
        fi
    done
done
[ "$leak" -eq 0 ] || { echo "Refusing to package." >&2; exit 1; }

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
git archive --prefix="$NAME/" HEAD | tar -x -C "$STAGE"
mkdir -p "$STAGE/$NAME/target/$TARGET/release"
install -m 0755 "$BIN_DIR/irgate" "$BIN_DIR/irgate-enroll" \
    "$STAGE/$NAME/target/$TARGET/release/"

mkdir -p "$DIST"
MTIME="$(git log -1 --format=%cI HEAD)"
tar -C "$STAGE" --sort=name --mtime="$MTIME" \
    --owner=0 --group=0 --numeric-owner \
    -czf "$DIST/$NAME.tar.gz" "$NAME"
(cd "$DIST" && sha256sum "$NAME.tar.gz" > SHA256SUMS)

echo ""
echo "Built from $(git rev-parse --short HEAD):"
ls -l "$DIST/$NAME.tar.gz"
cat "$DIST/SHA256SUMS"
