#!/usr/bin/env bash
# Build and exercise the packaged executable without ambient tools on PATH.
set -euo pipefail
cd "$(dirname "$0")/.."
package=$(nix build .#default --no-link --print-out-paths "$@")
findmatch=$(nix eval --raw .#devShells.x86_64-linux.default.FILE_ORGANIZER_FINDMATCH)
[[ "$package" == /nix/store/* && "$findmatch" == /nix/store/*/bin/findmatch ]]
env PATH=/nonexistent FILE_ORGANIZER_FINDMATCH=/nonexistent "$package/bin/file-organizer" --help
env PATH=/nonexistent "$package/bin/file-organizer" --version
env PATH=/nonexistent "$findmatch" --help > /dev/null
closure=$(nix-store --query --requisites "$package")
found=false
while IFS= read -r reference; do
  if [[ "$reference/bin/findmatch" == "$findmatch" ]]; then
    found=true
    break
  fi
done <<< "$closure"
if [[ "$found" != true ]]; then
  printf 'Pinned findmatch missing from package closure\n' >&2
  exit 1
fi
printf 'Pinned findmatch closure verified: %s\n' "$findmatch"
printf 'PATH-independent package verified: %s\n' "$package"
