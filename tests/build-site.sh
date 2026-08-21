#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

./scripts/build-site.sh

html="$repo_root/dist/site/index.html"

fail() {
    echo "build-site test: $1" >&2
    exit 1
}

test -f "$html" || fail "did not write index.html"
grep -q 'Fork me on GitHub' "$html" || fail "missing Fork me on GitHub badge"
grep -q 'https://github.com/UnburnTV/unburn"' "$html" || fail "fork badge should link to the GitHub repository"
grep -q 'macOS' "$html" || fail "missing macOS button"
grep -q 'Windows' "$html" || fail "missing Windows button"
grep -q 'coming soon' "$html" || fail "missing coming soon label"
grep -q 'Linux' "$html" || fail "missing Linux button"
grep -q 'https://github.com/UnburnTV/unburn/releases/latest' "$html" || fail "Linux button should link to GitHub releases"
if ! awk '
    /class="btn-icon"/ { icons += 1 }
    END { exit !(icons == 3) }
' "$html"; then
    fail "each download button must include an icon"
fi

# Download buttons belong under the page title, not above it.
if ! awk '
    /<h1/ { title = 1 }
    /class="downloads"/ { downloads = 1; if (!title) exit 1 }
    END { exit !(title && downloads) }
' "$html"; then
    fail "download buttons must appear after the rendered README title"
fi

# Coming-soon targets must not be links.
if awk '
    BEGIN { RS = "<"; FS = ">" }
    $1 ~ /^a[ \t\n]/ && $0 ~ /macOS|Windows/ { found = 1 }
    END { exit !found }
' "$html"; then
    fail "macOS or Windows coming-soon control was rendered as a link"
fi

test -f "$repo_root/dist/site/style.css" || fail "missing style.css"
test -f "$repo_root/dist/site/CNAME" || fail "missing CNAME"
if ! awk 'NR == 1 && $0 == "unburn.tv" { ok = 1 } END { exit !ok }' \
    "$repo_root/dist/site/CNAME"; then
    fail "CNAME must be unburn.tv"
fi
test -f "$repo_root/dist/site/docs/defects.jpg" || fail "missing docs/defects.jpg"
test -f "$repo_root/dist/site/docs/edit-mode.png" || fail "missing docs/edit-mode.png"

echo "[ok] Site build"
