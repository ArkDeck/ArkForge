#!/bin/bash
# Signs the complete ArkForge macOS release bundle: the canonical `arkforge`
# CLI, its native `arkforged` mechanics daemon, the published profiles, and a
# manifest binding every member byte (AFD-0003, CHG-2026-CLI).
#
# What this does NOT do, deliberately:
#
#   * notarize. The release pair is notarized with the outermost container that
#     ships it — for ArkDeck that is the archive its own packager submits. A
#     separate submission for a nested binary would produce a ticket nothing
#     staples;
#   * install, launch, or touch a device;
#   * install, launch, or rewrite the bundle. Consumers receive one immutable
#     `ArkForge.bundle` path and validate its manifest independently.
#
# The order below is fixed. No stage may be skipped or reordered, and a
# self-reported field never replaces an inspection: every property is read back
# out of the signed bytes with `codesign`, `file`, `otool` and the in-repo
# reader, not carried forward from the argument that asked for it.
set -euo pipefail

packaging_root="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$packaging_root/../.." && pwd)"
output_root="${ARKFORGE_PACKAGE_OUTPUT:-$repo_root/target/ArkForge.bundle}"
identity="${ARKFORGE_CODESIGN_IDENTITY:-}"
signing_prefix="${ARKFORGE_SIGNING_PREFIX:-com.arkforge}"

if [[ -z "$identity" ]]; then
  echo "ARKFORGE_CODESIGN_IDENTITY is required (a Developer ID Application identity)" >&2
  echo "without one there is nothing to sign with, and an ad-hoc signature is what" >&2
  echo "AD-011 spent an afternoon on: perfect digest, hung in dyld under quarantine" >&2
  exit 64
fi
if [[ -e "$output_root" ]]; then
  echo "output already exists, and this script never overwrites one: $output_root" >&2
  exit 73
fi

staging_root=""
cleanup() {
  if [[ -n "$staging_root" && -d "$staging_root" ]]; then
    rm -rf "$staging_root"
  fi
}
trap cleanup EXIT

# 1. build --------------------------------------------------------------------
cargo build --manifest-path "$repo_root/Cargo.toml" --release --offline \
  -p arkforged --bin arkforged -p arkforge-cli --bin arkforge
release_bin="$repo_root/target/release"

staging_root="$(mktemp -d "${TMPDIR:-/tmp}/arkforge-package.XXXXXX")"
stage="$staging_root/ArkForge.bundle"
macos_dir="$stage/Contents/MacOS"
resources_dir="$stage/Contents/Resources"
profiles_dir="$resources_dir/profiles"
mkdir -p "$macos_dir" "$profiles_dir"
cp "$release_bin/arkforged" "$macos_dir/arkforged"
cp "$release_bin/arkforge" "$macos_dir/arkforge"
cp "$repo_root/profiles/dayu200.yaml" "$profiles_dir/dayu200.yaml"
cp "$repo_root/profiles/dayu600.yaml" "$profiles_dir/dayu600.yaml"
chmod 700 "$macos_dir/arkforge" "$macos_dir/arkforged"
chmod 600 "$profiles_dir/dayu200.yaml" "$profiles_dir/dayu600.yaml"

# 2. architecture and dependency closure, before anything is signed ------------
for component in arkforge arkforged; do
  if ! file "$macos_dir/$component" | grep -q "arm64"; then
    echo "$component is not arm64: $(file "$macos_dir/$component")" >&2
    exit 65
  fi
  while read -r dylib; do
    case "$dylib" in
      /usr/lib/*|/System/Library/*) ;;
      *)
        echo "$component links $dylib, which is not a system library;" >&2
        echo "a release component may not depend on anything this host happens to have" >&2
        exit 65
        ;;
    esac
  done < <(otool -L "$macos_dir/$component" | tail -n +2 | awk '{print $1}')
done

# 3. sign both siblings with the empty entitlement dictionary ------------------
# `--deep` is never used to sign. Each item is signed by name so that what it
# carries is what this script chose, not what a traversal inferred.
for component in arkforge arkforged; do
  codesign --force --sign "$identity" --options runtime --timestamp \
    --identifier "$signing_prefix.$component" \
    --entitlements "$packaging_root/arkforged.entitlements" \
    "$macos_dir/$component"
done

# 4. independent read-back ----------------------------------------------------
# Two readers, on purpose: `codesign` is the system's answer, and the in-repo
# reader is the one the daemon will actually apply at bind time. A contract only
# one of them enforces is a contract that drifts.
for component in arkforge arkforged; do
  codesign --verify --strict --verbose=2 "$macos_dir/$component"
  entitlements="$(codesign -d --entitlements - --xml "$macos_dir/$component" 2>/dev/null | tail -1)"
  if [[ "$entitlements" == *"<key>"* ]]; then
    echo "$component came out carrying entitlements: $entitlements" >&2
    echo "the contract is an empty dictionary (AD-007)" >&2
    exit 65
  fi
  "$release_bin/arkforge" signing verify \
    --file "$macos_dir/$component" --mode release
done

# 5. one release manifest ------------------------------------------------------
# Digests are read from the signed bytes; signing changes a binary, so a
# pre-signing build digest cannot identify a release component. The manifest
# is deliberately outside its own member list to avoid a circular self-hash.
manifest="$resources_dir/arkforge-bundle.json"
{
  echo '{'
  echo '  "members": ['
  echo "    {\"bytes\": $(wc -c < "$macos_dir/arkforge" | tr -d ' '), \"path\": \"Contents/MacOS/arkforge\", \"role\": \"cli\", \"sha256\": \"$(shasum -a 256 "$macos_dir/arkforge" | cut -d' ' -f1)\"},"
  echo "    {\"bytes\": $(wc -c < "$macos_dir/arkforged" | tr -d ' '), \"path\": \"Contents/MacOS/arkforged\", \"role\": \"daemon\", \"sha256\": \"$(shasum -a 256 "$macos_dir/arkforged" | cut -d' ' -f1)\"},"
  echo "    {\"bytes\": $(wc -c < "$profiles_dir/dayu200.yaml" | tr -d ' '), \"path\": \"Contents/Resources/profiles/dayu200.yaml\", \"profileId\": \"org.openharmony.dayu200\", \"role\": \"profile\", \"sha256\": \"$(shasum -a 256 "$profiles_dir/dayu200.yaml" | cut -d' ' -f1)\"},"
  echo "    {\"bytes\": $(wc -c < "$profiles_dir/dayu600.yaml" | tr -d ' '), \"path\": \"Contents/Resources/profiles/dayu600.yaml\", \"profileId\": \"org.openharmony.dayu600\", \"role\": \"profile\", \"sha256\": \"$(shasum -a 256 "$profiles_dir/dayu600.yaml" | cut -d' ' -f1)\"}"
  echo '  ],'
  echo '  "schema": "arkforge.release-bundle/v1",'
  echo '  "version": "0.1.0"'
  echo '}'
} > "$manifest"

if find "$stage" -type l | grep -q .; then
  echo "ArkForge.bundle must not contain symbolic links" >&2
  exit 65
fi

mkdir -p "$(dirname "$output_root")"
mv "$stage" "$output_root"
rm -rf "$staging_root"
staging_root=""
trap - EXIT
echo "$output_root"
