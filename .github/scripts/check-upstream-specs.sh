#!/usr/bin/env bash
# Checks the upstream specifications this crate follows for changes since the
# versions pinned in .github/upstream-specs.json: new release tags, and commits
# on the default branch that touch the spec files.
#
#   check-upstream-specs.sh            print what changed (exit 0 either way)
#   check-upstream-specs.sh --issues   also open or update one GitHub issue per
#                                      changed spec, and close the issue of a
#                                      spec whose pin has caught up (needs gh
#                                      and GH_TOKEN; used by the workflow)
#   check-upstream-specs.sh --bump     move every pin to the current upstream,
#                                      after the crate has been brought in step
#
# Upstream is read with git over HTTPS, so no token is needed for it.

set -euo pipefail

mode="${1:-report}"
case "$mode" in
  report | --issues | --bump) ;;
  *)
    echo "usage: $0 [--issues | --bump]" >&2
    exit 2
    ;;
esac

root="$(cd "$(dirname "$0")/../.." && pwd)"
pins="$root/.github/upstream-specs.json"
label="upstream-spec"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

count="$(jq '.specs | length' "$pins")"
changed=0

for i in $(seq 0 $((count - 1))); do
  spec="$(jq -c ".specs[$i]" "$pins")"
  name="$(jq -r .name <<<"$spec")"
  repo="$(jq -r .repo <<<"$spec")"
  pattern="$(jq -r .tag_pattern <<<"$spec")"
  pinned_commit="$(jq -r .pinned.commit <<<"$spec")"
  pinned_tag="$(jq -r .pinned.tag <<<"$spec")"
  # A `while read` loop, not `mapfile`: macOS ships bash 3.2, which has no
  # `mapfile`, and the `--bump` step this script tells you to run is local.
  paths=()
  while IFS= read -r path; do paths+=("$path"); done < <(jq -r '.paths[]' <<<"$spec")

  # Commits and trees only: enough for the log, and quick even for a big repo.
  dir="$work/$i"
  git clone --quiet --filter=blob:none --no-checkout "https://github.com/$repo.git" "$dir"
  head="$(git -C "$dir" rev-parse HEAD)"
  branch="$(git -C "$dir" symbolic-ref --short HEAD)"

  # Release tags newer than the pinned one, by creation date: sorting by name
  # would put `2026-07-28-RC` after `2026-07-28`.
  tags="$(git -C "$dir" for-each-ref --sort=creatordate refs/tags \
    --format='%(creatordate:unix) %(refname:short)' |
    while read -r date tag; do
      if [[ $tag =~ $pattern ]]; then echo "$date $tag"; fi
    done)"
  pinned_date="$(awk -v t="$pinned_tag" '$2 == t { print $1 }' <<<"$tags")"
  new_tags="$(awk -v d="${pinned_date:-0}" '$1 > d { print $2 }' <<<"$tags")"
  latest_tag="$(tail -n 1 <<<"$tags" | cut -d' ' -f2)"

  if [ "$mode" = --bump ]; then
    jq --indent 2 --arg c "$head" --arg t "$latest_tag" \
      ".specs[$i].pinned = {commit: \$c, tag: \$t}" "$pins" >"$work/pins" && mv "$work/pins" "$pins"
    echo "$name: pinned to $head ($latest_tag)"
    continue
  fi

  if git -C "$dir" cat-file -e "$pinned_commit^{commit}" 2>/dev/null; then
    commits="$(git -C "$dir" log --format="- [\`%h\`](https://github.com/$repo/commit/%H) %ad %s" \
      --date=short "$pinned_commit..$head" -- "${paths[@]}")"
    missing=""
  else
    commits=""
    missing="The pinned commit \`$pinned_commit\` is no longer on \`$branch\` upstream (history was rewritten?), so the commits since it cannot be listed."
  fi

  if [ -z "$commits" ] && [ -z "$new_tags" ] && [ -z "$missing" ]; then
    echo "$name: unchanged since ${pinned_commit:0:7} ($pinned_tag)"
    state=unchanged
  else
    changed=1
    state=changed
    {
      echo "<!-- upstream-head: $head -->"
      echo "[$repo](https://github.com/$repo) has changed since the version this crate is pinned to (\`${pinned_commit:0:7}\`, release \`$pinned_tag\`)."
      echo
      if [ -n "$new_tags" ]; then
        echo "### New releases"
        echo
        while read -r tag; do
          echo "- [\`$tag\`](https://github.com/$repo/releases/tag/$tag)"
        done <<<"$new_tags"
        echo
      fi
      if [ -n "$commits" ]; then
        echo "### Commits touching the spec on \`$branch\`"
        echo
        echo "Watched paths: \`${paths[*]}\`"
        echo
        echo "$commits"
        echo
      fi
      [ -n "$missing" ] && echo "$missing" && echo
      echo "**Full diff:** https://github.com/$repo/compare/$pinned_commit...$head"
      echo
      echo "Once the crate is in step (or the change doesn't matter to it), run \`.github/scripts/check-upstream-specs.sh --bump\` and commit \`.github/upstream-specs.json\`. The next check closes this issue."
    } >"$work/body"
    echo "$name: CHANGED"
    sed '1d' "$work/body"
    echo
  fi

  [ "$mode" = --issues ] || continue

  title="Upstream spec changed: $name"
  issue="$(gh issue list --state open --label "$label" --limit 100 --json number,title,body |
    jq -c --arg t "$title" 'map(select(.title == $t)) | first // empty')"

  if [ "$state" = unchanged ]; then
    if [ -n "$issue" ]; then
      number="$(jq -r .number <<<"$issue")"
      gh issue close "$number" --reason completed \
        --comment "The pin in \`.github/upstream-specs.json\` has caught up with upstream, so this is done."
    fi
  elif [ -z "$issue" ]; then
    gh label create "$label" --color FBCA04 \
      --description "An upstream specification changed" 2>/dev/null || true
    gh issue create --title "$title" --label "$label" --body-file "$work/body"
  elif ! grep -qF "<!-- upstream-head: $head -->" <<<"$(jq -r .body <<<"$issue")"; then
    # Same open issue, newer upstream: refresh it and say so, which notifies.
    number="$(jq -r .number <<<"$issue")"
    gh issue edit "$number" --body-file "$work/body"
    gh issue comment "$number" --body "Upstream moved on again (now at \`${head:0:7}\`); the description above is updated."
  fi
done

if [ "$mode" = report ] && [ "$changed" = 1 ]; then
  echo "Some upstream specs changed; see above."
fi
