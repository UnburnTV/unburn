# Debian Package Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a versioned Ubuntu-compatible `.deb` for `vX.Y.Z` tags and verify its installation in tagged GitHub Actions runs.

**Architecture:** A repository shell script validates the release tag against `Cargo.toml`, builds the release binary, and assembles a minimal Debian package with `dpkg-deb`. A tag-only CI job invokes the script, installs the package, verifies the installed CLI version, and uploads the `.deb` as a workflow artifact.

**Tech Stack:** POSIX shell, Cargo, Debian `dpkg` tools, GitHub Actions.

## Global Constraints

- Every string emitted to a terminal must contain ASCII characters only.
- Work on the current feature branch.
- Do not add package-building dependencies.
- Do not install the package during local verification.
- Accept release tags in the exact `vX.Y.Z` form.
- Derive the Debian version by removing the leading `v`.
- Require the tag version to match the Cargo package version.

---

### Task 1: Debian Package Builder

**Files:**
- Create: `tests/build-deb.sh`
- Create: `scripts/build-deb.sh`
- Create: `packaging/unburn.desktop`

**Interfaces:**
- Consumes: one `vX.Y.Z` argument, Cargo package metadata, and the host Debian architecture.
- Produces: `dist/unburn_X.Y.Z_<architecture>.deb`.

- [ ] **Step 1: Write the failing package test**

Create `tests/build-deb.sh`:

```sh
#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

extract_dir=$(mktemp -d)
log_file=$(mktemp)
trap 'rm -rf "$extract_dir" "$log_file"' EXIT HUP INT TERM

version=$(cargo metadata --no-deps --format-version 1 |
    python3 -c 'import json, sys; print(json.load(sys.stdin)["packages"][0]["version"])')
tag="v$version"
architecture=$(dpkg --print-architecture)
package="dist/unburn_${version}_${architecture}.deb"

if ./scripts/build-deb.sh "$version" >"$log_file" 2>&1; then
    echo "build-deb test: accepted a tag without the v prefix" >&2
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
echo "build-deb test: passed"
```

- [ ] **Step 2: Run the package test and verify it fails**

Run: `tests/build-deb.sh`

Expected: FAIL because `scripts/build-deb.sh` does not exist.

- [ ] **Step 3: Implement the package builder**

Create `scripts/build-deb.sh` with strict shell error handling. Validate the tag, compare its stripped version to the package version returned by `cargo +nightly metadata`, run `cargo +nightly build --locked --release --bin unburn`, stage the binary, desktop entry, and copyright metadata under a temporary package root, write Debian control metadata, and call:

```sh
dpkg-deb --root-owner-group --build "$package_root" "$output"
```

The package declares the linked and dynamically loaded Ubuntu runtime libraries used by the Wayland and X11 GUI backends.

- [ ] **Step 4: Run the package test and verify it passes without installing**

Run: `tests/build-deb.sh`

Expected: PASS and a package under `dist/`; no package installation occurs.

### Task 2: Tagged CI Packaging

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `README.md`

**Interfaces:**
- Consumes: Git tags matching `v[0-9]+.[0-9]+.[0-9]+` and `scripts/build-deb.sh`.
- Produces: an installed-and-smoke-tested package uploaded as a GitHub Actions artifact.

- [ ] **Step 1: Add a static workflow test**

Extend `tests/build-deb.sh` with:

```sh
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
```

- [ ] **Step 2: Run the workflow test and verify it fails**

Run: `tests/build-deb.sh`

Expected: FAIL because the workflow does not yet define tag packaging.

- [ ] **Step 3: Add the tag packaging job**

Add `v[0-9]+.[0-9]+.[0-9]+` to the push tag trigger. Add a package job gated by `startsWith(github.ref, 'refs/tags/v')` that installs build dependencies, installs Rust nightly, runs `scripts/build-deb.sh "${{ github.ref_name }}"`, installs `dist/*.deb` with `apt-get`, verifies `unburn --version` equals the tag version, and uploads `dist/*.deb`.

- [ ] **Step 4: Document local package construction**

Add a README section showing:

```sh
./scripts/build-deb.sh v0.1.0
```

Explain that the output is written to `dist/` and that installation testing is performed by CI for tagged revisions.

- [ ] **Step 5: Run full verification**

Run:

```sh
tests/build-deb.sh
cargo +nightly fmt --all -- --check
cargo +nightly clippy --all-targets --all-features -- -D warnings
cargo +nightly test --all-targets --all-features
```

Expected: every command exits 0. The package test inspects but does not install the `.deb` locally.
