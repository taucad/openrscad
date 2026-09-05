#!/usr/bin/env bash
# Prove an artifact download actually landed.
#
# `actions/download-artifact` has been observed to log the redirect and the
# "Starting download" line, write nothing, and still exit zero. The consumer then
# dies somewhere far downstream against a path it never chose. Assert the landing
# here, at the boundary, and say what is missing (napi-architecture-policy §5).
#
# ARTIFACT      - name or pattern the caller downloaded, for the message only.
# ARTIFACT_PATH - directory the download targeted.
# EXPECT        - optional whitespace-separated paths, relative to ARTIFACT_PATH,
#                 that the artifact must contain.

set -euo pipefail

fail() {
  echo "::error::artifact '$ARTIFACT' did not land in '$ARTIFACT_PATH': $1"
  if [[ -d "$ARTIFACT_PATH" ]]; then
    echo "contents of $ARTIFACT_PATH:"
    ls -l "$ARTIFACT_PATH"
  else
    echo "$ARTIFACT_PATH does not exist"
  fi
  exit 1
}

[[ -d "$ARTIFACT_PATH" ]] || fail "the download created no directory"

# A glob rather than `find`: this runs on the Windows rows too, where a bare
# `find` can resolve to System32's unrelated `find.exe`.
shopt -s nullglob dotglob
landed=("$ARTIFACT_PATH"/*)
[[ ${#landed[@]} -gt 0 ]] || fail "the directory is empty"

# Word splitting is the point: EXPECT carries a whitespace-separated list.
# shellcheck disable=SC2086
for entry in ${EXPECT:-}; do
  [[ -e "$ARTIFACT_PATH/$entry" ]] || fail "$entry is missing"
done

echo "artifact '$ARTIFACT' landed in '$ARTIFACT_PATH' (${#landed[@]} top-level entries)"
