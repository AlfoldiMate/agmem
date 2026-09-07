#!/usr/bin/env bash
# Bump the workspace version and everything that has to carry the same
# number, in the working tree, and print the new version.
#
#   bump-version.sh patch|minor|major
#
# Touches: [workspace.package].version and the three internal
# [workspace.dependencies] requirements in Cargo.toml, Cargo.lock, and the
# two plugin manifests (the plugin's hooks are `agmem hook`, so it is only
# ever as new as the binary; a test in agmem-server fails when they lag).
# Nothing is committed here — the release workflow does that.
set -euo pipefail

bump=${1:?patch|minor|major}
case "$bump" in patch|minor|major) ;; *) echo "bad bump: $bump" >&2; exit 2 ;; esac

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

old=$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | select(.name == "agmem-server") | .version')
IFS=. read -r major minor patch <<<"$old"
case "$bump" in
  major) new="$((major + 1)).0.0" ;;
  minor) new="$major.$((minor + 1)).0" ;;
  patch) new="$major.$minor.$((patch + 1))" ;;
esac

# The workspace version and the internal path dependencies' requirements.
# Both patterns are anchored to their exact current text, and each edit is
# checked afterwards, because a silent no-op here ships the old number.
sed -i.bak -E "s/^version = \"$old\"$/version = \"$new\"/" Cargo.toml
sed -i.bak -E "s/^(agmem-[a-z]+ = \{ path = \"crates\/agmem-[a-z]+\", version = )\"$old\" \}$/\1\"$new\" }/" Cargo.toml
rm Cargo.toml.bak
[ "$(grep -c "^version = \"$new\"$" Cargo.toml)" = 1 ] || { echo "Cargo.toml: workspace version not at $new" >&2; exit 1; }
[ "$(grep -c "version = \"$new\" }$" Cargo.toml)" = 3 ] || { echo "Cargo.toml: internal dependencies not at $new" >&2; exit 1; }

for f in plugin/.claude-plugin/plugin.json .claude-plugin/marketplace.json; do
  sed -i.bak -E "s/\"version\": \"$old\"/\"version\": \"$new\"/" "$f"
  rm "$f.bak"
  grep -q "\"version\": \"$new\"" "$f" || { echo "$f: version not pinned" >&2; exit 1; }
done

# Refresh the lock file's entries for the workspace members only; no
# dependency moves. --offline: the registry index is not needed for that.
cargo update --workspace --offline --quiet
for crate in agmem-core agmem-store agmem-embed agmem-server; do
  grep -A1 "^name = \"$crate\"$" Cargo.lock | grep -q "^version = \"$new\"$" \
    || { echo "Cargo.lock: $crate not at $new" >&2; exit 1; }
done

echo "$new"
