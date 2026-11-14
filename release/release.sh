#!/usr/bin/env bash
# Compila Linux e Windows num container e publica a release no GitHub.
#   release/release.sh           compila, cria a tag vX.Y.Z do Cargo.toml, push e release
#   release/release.sh --build   só compila, em target/docker/dist
# Antes: subir `version` no Cargo.toml e commitar.
set -euo pipefail
cd "$(dirname "$0")/.."

die() { echo "release: $*" >&2; exit 1; }
publica=1
[[ ${1:-} == --build ]] && publica=0

tag="v$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
if ((publica)); then
    [[ $(git branch --show-current) == main ]] || die "fora da main"
    [[ -z $(git status --porcelain) ]] || die "árvore com mudança sem commit"
    git fetch -q --tags origin
    git merge-base --is-ancestor origin/main main || die "main local atrás da origin/main"
    ! git rev-parse -q --verify "refs/tags/$tag" >/dev/null ||
        die "tag $tag já existe: sobe a version no Cargo.toml"
fi

# Registry do cargo e CRT/SDK da Microsoft ficam fora do container, pra não baixar de novo.
cache="${XDG_CACHE_HOME:-$HOME/.cache}/concord-build"
mkdir -p "$cache"/{registry,git,home}
docker build -t concord-client-build release/
# O cargo-xwin guarda o CRT e o SDK em $HOME/.cache/cargo-xwin.
docker run --rm --user "$(id -u):$(id -g)" \
    -v "$PWD:/src" -w /src \
    -v "$cache/registry:/opt/cargo/registry" -v "$cache/git:/opt/cargo/git" \
    -v "$cache/home:/cache/home" -e HOME=/cache/home \
    concord-client-build bash -ec '
        CC=clang-21 CXX=clang++-21 cargo build --release --target-dir target/docker
        cargo xwin build --release --target x86_64-pc-windows-msvc --target-dir target/docker
    '

# Os nomes que o autoupdater procura (`src/update.rs`).
dist=target/docker/dist
mkdir -p "$dist"
cp target/docker/release/concord-client "$dist/concord-client-linux-x86_64"
cp target/docker/x86_64-pc-windows-msvc/release/concord-client.exe \
    "$dist/concord-client-windows-x86_64.exe"
sha256sum "$dist"/*
((publica)) || exit 0

git tag "$tag"
git push origin main "$tag"
gh release create "$tag" "$dist"/* --title "$tag" --generate-notes
