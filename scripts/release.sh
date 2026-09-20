#!/usr/bin/env bash
set -Eeuo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/release.sh extension <x.y.z> [--push] [--skip-checks]

Updates the extension when needed, creates a release commit and an annotated tag.
Extension tags are vX.Y.Z-standalone. Go service images use .github/workflows/service-image.yml.
The tag is pushed only when --push is supplied.
EOF
}

if (( $# == 1 )) && [[ "$1" == "-h" || "$1" == "--help" ]]; then
  usage
  exit 0
fi

if (( $# < 2 )); then
  usage >&2
  exit 2
fi

artifact=$1
version=$2
shift 2
push=false
skip_checks=false

while (( $# > 0 )); do
  case "$1" in
    --push) push=true ;;
    --skip-checks) skip_checks=true ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Version must use x.y.z form: $version" >&2
  exit 2
fi

case "$artifact" in
  service)
    echo "This script manages extension releases only." >&2
    echo "Publish Go service images with .github/workflows/service-image.yml; see service/docs/README.md." >&2
    exit 2
    ;;
  extension)
    tag="v${version}-standalone"
    version_files=(extension-standalone/manifest.json extension-standalone/popup.html)
    ;;
  *)
    echo "Artifact must be extension: $artifact" >&2
    exit 2
    ;;
esac

for command in git perl jq; do
  command -v "$command" >/dev/null || {
    echo "Required command not found: $command" >&2
    exit 1
  }
done
command -v node >/dev/null || { echo "Required command not found: node" >&2; exit 1; }

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

push_release() {
  local branch
  branch="$(git branch --show-current)"
  if [[ -n "$branch" ]]; then
    git push origin "$branch" "$tag"
  else
    git push origin "$tag"
  fi
}

if git rev-parse --verify --quiet "refs/tags/${tag}" >/dev/null; then
  existing_commit="$(git rev-list -n 1 "$tag")"
  head_commit="$(git rev-parse HEAD)"
  if [[ "$push" == true && "$existing_commit" == "$head_commit" ]]; then
    push_release
    echo "Tag ${tag} already existed locally and was pushed."
    exit 0
  fi
  echo "Tag already exists: ${tag}" >&2
  echo "It points to ${existing_commit}; refusing to overwrite it." >&2
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "Release requires a clean worktree; commit or stash existing changes first." >&2
  exit 1
fi

update_extension_version() {
  RELEASE_VERSION="$version" perl -0pi -e \
    's/^(\s+"version":\s+")[^"]+("\s*,\s*\n)/$1 . $ENV{RELEASE_VERSION} . $2/em' \
    extension-standalone/manifest.json
  RELEASE_VERSION="$version" perl -0pi -e \
    's/(data-i18n="footer">)v[0-9]+\.[0-9]+\.[0-9]+(\s*\|)/$1 . "v" . $ENV{RELEASE_VERSION} . $2/em' \
    extension-standalone/popup.html
}

validate_extension_version() {
  jq -e --arg version "$version" '.version == $version' extension-standalone/manifest.json >/dev/null
}

run_extension_checks() {
  node --check extension-standalone/background.js
  node --check extension-standalone/nga-api.js
  node --check extension-standalone/popup.js
  node --check extension-standalone/posts.js
  node --check extension-standalone/i18n.js
  node --check extension-standalone/thread-config.mjs
  node --test extension-standalone/static.test.mjs
}

update_extension_version
validate_extension_version
if [[ "$skip_checks" != true ]]; then
  run_extension_checks
fi

git diff --check
if ! git diff --quiet -- "${version_files[@]}"; then
  git add "${version_files[@]}"
  git commit -m "release: ${artifact} ${tag}"
else
  echo "Version files already contain ${version}; creating the tag without a new commit."
fi
git tag --annotate "$tag" --message "Release ${tag}"

if [[ "$push" == true ]]; then
  push_release
fi

echo "Created ${tag}"
if [[ "$push" != true ]]; then
  branch="$(git branch --show-current)"
  if [[ -n "$branch" ]]; then
    echo "Push it with: git push origin ${branch} ${tag}"
  else
    echo "Push it with: git push origin ${tag}"
  fi
fi
