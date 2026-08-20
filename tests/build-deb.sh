#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

extract_dir=$(mktemp -d)
log_file=$(mktemp)
trap 'rm -rf "$extract_dir" "$log_file"' EXIT HUP INT TERM

version=$(cargo +nightly metadata --locked --no-deps --format-version 1 |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["packages"][0]["version"])')
tag="v$version"
architecture=$(dpkg --print-architecture)
package="dist/unburn_${version}_${architecture}.deb"

if ./scripts/build-deb.sh "$version" >"$log_file" 2>&1; then
    echo "build-deb test: accepted a tag without the v prefix" >&2
    exit 1
fi
if ./scripts/build-deb.sh 'v1.2.non-ascii-é' >"$log_file" 2>&1; then
    echo "build-deb test: accepted a malformed tag" >&2
    exit 1
fi
if LC_ALL=C tr -d '\000-\177' <"$log_file" |
    awk 'length { found = 1 } END { exit !found }'; then
    echo "build-deb test: emitted non-ASCII output for a malformed tag" >&2
    exit 1
fi
if ./scripts/build-deb.sh v999.999.999 >"$log_file" 2>&1; then
    echo "build-deb test: accepted a version that differs from Cargo.toml" >&2
    exit 1
fi

./scripts/build-deb.sh "$tag"
test -f "$package"
test "$(dpkg-deb --field "$package" Package)" = "unburn"
test "$(dpkg-deb --field "$package" Version)" = "$version"
test "$(dpkg-deb --field "$package" Architecture)" = "$architecture"
expected_dependencies="libc6, libgcc-s1, libegl1, libgl1, libwayland-client0, libwayland-egl1, libx11-6, libx11-xcb1, libxcursor1, libxi6, libxkbcommon0, libxkbcommon-x11-0, libxrender1"
test "$(dpkg-deb --field "$package" Depends)" = "$expected_dependencies"

dpkg-deb --extract "$package" "$extract_dir"
test -x "$extract_dir/usr/bin/unburn"
test -f "$extract_dir/usr/share/applications/unburn.desktop"
test -f "$extract_dir/usr/share/doc/unburn/copyright"

assert_workflow_contains() {
    needle=$1
    if ! awk -v needle="$needle" 'index($0, needle) { found = 1 } END { exit !found }' \
        .github/workflows/ci.yml; then
        echo "build-deb test: workflow is missing: $needle" >&2
        exit 1
    fi
}

assert_workflow_contains '- "v[0-9]+.[0-9]+.[0-9]+"'
assert_workflow_contains './scripts/build-deb.sh "${{ github.ref_name }}"'
assert_workflow_contains 'sudo apt-get install -y ./dist/*.deb'
assert_workflow_contains 'installed_version=$(unburn --version)'
assert_workflow_contains 'uses: actions/upload-artifact@v4'

echo "build-deb test: passed"
