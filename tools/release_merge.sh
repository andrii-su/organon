#!/usr/bin/env bash
# Publish one patch release for a successfully tested pull request merge.
set -euo pipefail

: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${GH_TOKEN:?GH_TOKEN is required}"

root="$(git rev-parse --show-toplevel)"
cd "$root"

pulls="$(gh api "repos/$GITHUB_REPOSITORY/commits/$GITHUB_SHA/pulls")"
pr="$(python3 tools/release_version.py merged-pr --sha "$GITHUB_SHA" <<< "$pulls")"
if [[ -z "$pr" ]]; then
  echo "No merged main pull request for $GITHUB_SHA; skipping release."
  exit 0
fi

wait_for_workflow() {
  local workflow="$1" state=pending runs
  echo "Waiting for $workflow on merged PR #$pr ($GITHUB_SHA)."
  for attempt in {1..60}; do
    runs="$(gh api "repos/$GITHUB_REPOSITORY/actions/workflows/$workflow/runs?head_sha=$GITHUB_SHA&event=push&per_page=20")"
    state="$(python3 tools/release_version.py ci-state --sha "$GITHUB_SHA" <<< "$runs")"
    if [[ "$state" != pending ]]; then
      break
    fi
    sleep 10
  done
  if [[ "$state" != success ]]; then
    echo "$workflow did not pass for $GITHUB_SHA (state: $state)." >&2
    return 1
  fi
}

wait_for_workflow ci.yml
wait_for_workflow security.yml

git fetch --force origin 'refs/tags/*:refs/tags/*'
tag="$(python3 tools/release_version.py find-tag --merge-sha "$GITHUB_SHA")"
if [[ -z "$tag" ]]; then
  version="$(python3 tools/release_version.py next)"
  tag="v$version"
  python3 tools/release_version.py apply "$version"
  cargo metadata --locked --no-deps --format-version 1 > /dev/null
  uv lock --check
  git config user.name 'github-actions[bot]'
  git config user.email '41898282+github-actions[bot]@users.noreply.github.com'
  git add Cargo.toml Cargo.lock pyproject.toml uv.lock
  git commit --allow-empty -m "chore(release): $tag"
  git tag -a "$tag" -m "Release $tag"
  git push origin "refs/tags/$tag"
fi

# Tag pushes made with GITHUB_TOKEN do not trigger push workflows. Explicit
# dispatch on the tag ref runs the existing release and image publishers.
gh workflow run release.yml --ref "$tag"
gh workflow run docker.yml --ref "$tag"
echo "Dispatched Release and Docker for $tag (PR #$pr)."
