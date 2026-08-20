#!/bin/bash
# Signs the ArkForge half of the macOS release: the native `arkforged` daemon,
# as nested code ready to be embedded by the container that ships it
# (AFD-0003, NRU-004).
#
# What this does NOT do, deliberately:
#
#   * notarize. Nested code is notarized with the outermost container it ships
#     inside — for ArkDeck that is the archive its own packager submits. A
#     separate submission for a nested binary would produce a ticket nothing
#     staples;
#   * install, launch, or touch a device;
#   * decide where the container puts these files. It produces one signed
#     binary and a receipt; embedding is the container's contract.
#
# The order below is fixed. No stage may be skipped or reordered, and a
# self-reported field never replaces an inspection: every property is read back
# out of the signed bytes with `codesign`, `file`, `otool` and the in-repo
# reader, not carried forward from the argument that asked for it.
set -euo pipefail

packaging_root="$(cd "$(dirname "$0")" && pwd)"
repo_root="$(cd "$packaging_root/../.." && pwd)"
output_root="${ARKFORGE_PACKAGE_OUTPUT:-$repo_root/target/arkforge-macos-release}"
identity="${ARKFORGE_CODESIGN_IDENTITY:-}"
signing_prefix="${ARKFORGE_SIGNING_PREFIX:-com.arkdeck.agentd}"

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
  -p arkforged --bin arkforged --bin arkforge
release_bin="$repo_root/target/release"

staging_root="$(mktemp -d "${TMPDIR:-/tmp}/arkforge-package.XXXXXX")"
stage="$staging_root/ArkForge"
mkdir -p "$stage"
cp "$release_bin/arkforged" "$stage/arkforged"
chmod 700 "$stage/arkforged"

# 2. architecture and dependency closure, before anything is signed ------------
for component in arkforged; do
  if ! file "$stage/$component" | grep -q "arm64"; then
    echo "$component is not arm64: $(file "$stage/$component")" >&2
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
  done < <(otool -L "$stage/$component" | tail -n +2 | awk '{print $1}')
done

# 3. sign with the daemon's empty entitlement dictionary -----------------------
# `--deep` is never used to sign. Each item is signed by name so that what it
# carries is what this script chose, not what a traversal inferred.
codesign --force --sign "$identity" --options runtime --timestamp \
  --identifier "$signing_prefix.arkforged" \
  --entitlements "$packaging_root/arkforged.entitlements" \
  "$stage/arkforged"

# 4. independent read-back ----------------------------------------------------
# Two readers, on purpose: `codesign` is the system's answer, and the in-repo
# reader is the one the daemon will actually apply at bind time. A contract only
# one of them enforces is a contract that drifts.
for component in arkforged; do
  codesign --verify --strict --verbose=2 "$stage/$component"
  entitlements="$(codesign -d --entitlements - --xml "$stage/$component" 2>/dev/null | tail -1)"
  if [[ "$entitlements" == *"<key>"* ]]; then
    echo "$component came out carrying entitlements: $entitlements" >&2
    echo "the contract is an empty dictionary (AD-007)" >&2
    exit 65
  fi
  "$release_bin/arkforge" signing verify \
    --file "$stage/$component" --mode release
done

# 5. receipt ------------------------------------------------------------------
# The digest is read from the signed bytes; signing changes the binary, so a
# pre-signing build digest cannot identify the release component.
receipt="$stage/package-receipt.json"
{
  echo "{"
  echo "  \"contract\": \"docs/decisions/AFD-0003-arkforged-signing-packaging.md\","
  echo "  \"signingPrefix\": \"$signing_prefix\","
  echo "  \"components\": {"
  echo "    \"arkforged\": {"
  echo "      \"signedSHA256\": \"$(shasum -a 256 "$stage/arkforged" | cut -d' ' -f1)\","
  echo "      \"identifier\": \"$signing_prefix.arkforged\""
  echo "    }"
  echo "  },"
  echo "  \"notarization\": \"not performed here; nested code is notarized with the container that ships it\""
  echo "}"
} > "$receipt"

mkdir -p "$(dirname "$output_root")"
mv "$stage" "$output_root"
rm -rf "$staging_root"
staging_root=""
trap - EXIT
echo "$output_root"
