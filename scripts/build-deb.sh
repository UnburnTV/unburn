#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "Usage: $0 vX.Y.Z" >&2
    exit 2
fi

tag=$1
if ! printf '%s\n' "$tag" |
    awk '/^v[0-9]+\.[0-9]+\.[0-9]+$/ { valid = 1 } END { exit !valid }'; then
    echo "build-deb: release tag must have the form vX.Y.Z: $tag" >&2
    exit 2
fi
version=${tag#v}

for command_name in cargo dpkg dpkg-deb python3; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "build-deb: required command not found: $command_name" >&2
        exit 1
    fi
done

metadata=$(cargo +nightly metadata --locked --no-deps --format-version 1)
manifest_version=$(printf '%s' "$metadata" | python3 -c '
import json
import sys

packages = json.load(sys.stdin)["packages"]
print(next(package["version"] for package in packages if package["name"] == "unburn"))
')
target_dir=$(printf '%s' "$metadata" |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')

if [ "$version" != "$manifest_version" ]; then
    echo "build-deb: tag version $version does not match Cargo.toml version $manifest_version" >&2
    exit 2
fi

architecture=$(dpkg --print-architecture)
output_dir=dist
output="$output_dir/unburn_${version}_${architecture}.deb"
package_root=$(mktemp -d)
trap 'rm -rf "$package_root"' EXIT HUP INT TERM

echo "Building unburn $version for $architecture"
cargo +nightly build --locked --release --bin unburn

install -Dm755 "$target_dir/release/unburn" "$package_root/usr/bin/unburn"
install -Dm644 packaging/unburn.desktop \
    "$package_root/usr/share/applications/unburn.desktop"
mkdir -p "$package_root/DEBIAN" "$output_dir"

cat >"$package_root/DEBIAN/control" <<EOF
Package: unburn
Version: $version
Section: utils
Priority: optional
Architecture: $architecture
Maintainer: unburn maintainers <noreply@unburn.tv>
Depends: libc6, libgcc-s1, libxkbcommon0
Homepage: https://unburn.tv
Description: Display uniformity compensation overlay
 Corrects OLED and LCD brightness defects with a configurable overlay.
EOF

rm -f "$output"
dpkg-deb --root-owner-group --build "$package_root" "$output"
echo "Package created: $output"
