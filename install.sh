#!/usr/bin/env bash
set -euo pipefail

fail() {
  printf 'Solaris plugins: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage: bash install.sh --directory SERVER_PLUGIN_DIR [PACKAGE ...]' \
    'Run from a downloaded checkout; no build or network access is needed.' \
    'With no package names, installs the five standard server-only packages.' \
    'Existing packages are never replaced. Stop the server before installing.'
}

destination=''
packages=()
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --directory)
      [[ "$#" -ge 2 && -n "$2" ]] || fail '--directory requires a path'
      destination="$2"
      shift 2
      ;;
    --help|-h) usage; exit 0 ;;
    --*) fail "unknown option: $1" ;;
    *) packages+=("$1"); shift ;;
  esac
done
[[ -n "$destination" ]] || { usage >&2; fail '--directory is required'; }

source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
if [[ "${#packages[@]}" -eq 0 ]]; then
  packages=(solaris-permissions solaris-essentials solaris-economy solaris-towns solaris-audit)
fi

declare -A selected=()
for package in "${packages[@]}"; do
  [[ "$package" =~ ^[a-z0-9][a-z0-9_-]*$ ]] || fail "invalid package name: $package"
  [[ -z "${selected[$package]+present}" ]] || fail "duplicate package: $package"
  selected[$package]=1
  [[ -f "$source_dir/$package/plugin.toml" && -f "$source_dir/$package/main.lua" ]] ||
    fail "not a deployable package in this checkout: $package"
  [[ ! -e "$destination/$package" && ! -L "$destination/$package" ]] ||
    fail "package already exists; no files replaced: $destination/$package"
done

mkdir -p -- "$destination"
destination="$(cd -- "$destination" && pwd)"
staging="$(mktemp -d "$(dirname -- "$destination")/.solaris-plugins.XXXXXX")"
trap 'rm -rf -- "$staging"' EXIT

for package in "${packages[@]}"; do
  cp -R -- "$source_dir/$package" "$staging/$package"
done
for package in "${packages[@]}"; do
  mv -T -n -- "$staging/$package" "$destination/$package"
  [[ ! -e "$staging/$package" ]] || fail "package appeared during installation: $destination/$package"
  printf 'Installed %s into %s\n' "$package" "$destination/$package"
done

printf '\nSet [plugins].directory to %s and include these ids in plugins.expected:\n' "$destination"
printf '  %s\n' "${packages[@]}"
printf '%s\n' 'Keep any other deployed ids in that list. Run solaris --check --config server.toml before starting.'
