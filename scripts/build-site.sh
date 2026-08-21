#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out="$repo_root/dist/site"

if ! command -v pandoc >/dev/null 2>&1; then
    echo "build-site: required command not found: pandoc" >&2
    exit 1
fi

rm -rf "$out"
mkdir -p "$out/docs" "$out/assets"

pandoc "$repo_root/README.md" \
    --from gfm \
    --to html5 \
    --standalone \
    --template="$repo_root/site/template.html" \
    --lua-filter="$repo_root/site/insert-downloads.lua" \
    --metadata title=unburn \
    --output="$out/index.html"

cp "$repo_root/site/style.css" "$out/style.css"
cp "$repo_root/site/CNAME" "$out/CNAME"
cp "$repo_root/assets/logo.png" "$out/assets/logo.png"

# Keep the docs/ prefix used by README image paths.
for image in "$repo_root"/docs/*.png "$repo_root"/docs/*.jpg "$repo_root"/docs/*.jpeg "$repo_root"/docs/*.gif "$repo_root"/docs/*.svg "$repo_root"/docs/*.webp
do
    if [ -f "$image" ]; then
        cp "$image" "$out/docs/"
    fi
done

# The generated tree is complete HTML; Jekyll should not process it.
touch "$out/.nojekyll"

echo "Site written to $out"
