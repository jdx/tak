#!/usr/bin/env bash
set -euo pipefail

[[ -f ${ARTIFACT:?} ]] || {
  echo "::error::Tak artifact does not exist: $ARTIFACT"
  exit 1
}
[[ -n ${EXPECT_REV:?} ]] || {
  echo "::error::expect must name a trusted revision"
  exit 1
}
[[ -n ${GH_TOKEN:?} ]] || {
  echo "::error::token is required"
  exit 1
}

# Pass authentication as an ephemeral Git configuration inherited by Tak's git
# subprocesses. Nothing is written to .git/config or a credential helper.
basic="$(printf 'x-access-token:%s' "$GH_TOKEN" | base64 | tr -d '\r\n')"
export GIT_CONFIG_COUNT=1
export GIT_CONFIG_KEY_0=http.https://github.com/.extraheader
export GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $basic"

"${TAK_BIN:?}" artifact publish "$ARTIFACT" --expect "$EXPECT_REV" --remote "${REMOTE:-origin}"
