#!/bin/sh
set -eu

if [ "$#" -ne 1 ]; then
    echo "Usage: build-deb.sh vX.Y.Z" >&2
    exit 2
fi

tag=$1
if ! printf '%s\n' "$tag" |
    awk '/^v[0-9]+\.[0-9]+\.[0-9]+$/ { valid = 1 } END { exit !valid }'; then
    echo "build-deb: release tag must have the form vX.Y.Z" >&2
    exit 2
fi
version=${tag#v}

for command_name in cargo dpkg dpkg-deb python3; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "build-deb: required command not found: $command_name" >&2
        exit 1
    fi
done

metadata=$(cargo metadata --locked --no-deps --format-version 1)
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
cargo build --locked --release --bin unburn

install -Dm755 "$target_dir/release/unburn" "$package_root/usr/bin/unburn"
install -Dm644 packaging/unburn.desktop \
    "$package_root/usr/share/applications/unburn.desktop"
install -Dm644 packaging/copyright \
    "$package_root/usr/share/doc/unburn/copyright"
mkdir -p "$package_root/DEBIAN" "$output_dir"

cat >"$package_root/DEBIAN/control" <<EOF
Package: unburn
Version: $version
Section: utils
Priority: optional
Architecture: $architecture
Maintainer: unburn maintainers <noreply@unburn.tv>
Depends: libc6, libgcc-s1, libegl1, libgl1, libwayland-client0, libwayland-egl1, libx11-6, libx11-xcb1, libxcursor1, libxi6, libxkbcommon0, libxkbcommon-x11-0, libxrender1
Homepage: https://unburn.tv
Description: Display uniformity compensation overlay
 Corrects OLED and LCD brightness defects with a configurable overlay.
EOF

rm -f "$output"
dpkg-deb --root-owner-group --build "$package_root" "$output"
echo "Package created: $output"
