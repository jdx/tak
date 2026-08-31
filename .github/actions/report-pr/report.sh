#!/usr/bin/env bash
set -euo pipefail

[[ ${HEAD_SHA:-} =~ ^[0-9a-f]{40}$ ]] || {
  echo "::error::head-sha must be a full lowercase commit SHA"
  exit 1
}
[[ ${PR_NUMBER:-} =~ ^[1-9][0-9]*$ ]] || {
  echo "::error::pr-number must be a positive integer"
  exit 1
}
[[ ${MARKER:-} =~ ^\<!--[a-zA-Z0-9_:/.-]+--\>$ ]] || {
  echo "::error::marker must be a simple HTML comment without whitespace"
  exit 1
}
[[ -n ${GH_TOKEN:-} && -n ${GITHUB_REPOSITORY:-} ]] || {
  echo "::error::token and GITHUB_REPOSITORY are required"
  exit 1
}

directory=${ARTIFACT_DIRECTORY:?}
report="$directory/report.md"
status_file="$directory/status"
shas="$directory/shas"
[[ -f $report && ! -L $report \
  && -f $status_file && ! -L $status_file \
  && -f $shas && ! -L $shas ]] || {
  echo "::error::artifact directory must contain regular, non-symlink report.md, status, and shas files"
  exit 1
}
[[ $(wc -c < "$report") -le 60000 ]] || {
  echo "::error::report.md exceeds the 60000-byte comment limit"
  exit 1
}
[[ $(wc -c < "$status_file") -le 16 && $(wc -c < "$shas") -le 128 ]] || {
  echo "::error::artifact metadata exceeds its size limit"
  exit 1
}

status=$(tr -d '[:space:]' < "$status_file")
[[ $status =~ ^[0-9]{1,3}$ && $status -le 255 ]] || {
  echo "::error::status must contain one exit status from 0 through 255"
  exit 1
}
read -r artifact_head base_sha extra < "$shas" || true
[[ -z ${extra:-} && ${artifact_head:-} == "${HEAD_SHA:0:12}" ]] || {
  echo "::error::artifact head does not match the independently validated head SHA"
  exit 1
}
[[ ${base_sha:-} =~ ^[0-9a-f]{12}$ ]] || base_sha=unknown

if [[ $status -eq 0 ]]; then
  conclusion=success
  title='Instruction counts passed'
else
  conclusion=failure
  title='Instruction-count regression or measurement failure'
fi
run_url="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID:?}"

# Create the check first: a comment API failure must not hide the actual gate
# result from the pull request's head commit.
jq -n --arg name "${CHECK_NAME:-perf / instruction-count}" --arg head_sha "$HEAD_SHA" \
  --arg conclusion "$conclusion" --arg title "$title" --arg details_url "$run_url" \
  --rawfile summary "$report" \
  '{name:$name,head_sha:$head_sha,status:"completed",conclusion:$conclusion,details_url:$details_url,output:{title:$title,summary:$summary}}' \
  | gh api --method POST "repos/$GITHUB_REPOSITORY/check-runs" --input - >/dev/null

{
  printf '%s\n' "$MARKER"
  printf '### Instruction counts\n\n'
  cat "$report"
  # shellcheck disable=SC2016 # Markdown code spans, not shell expansion.
  printf '\n<sub>`%s` vs `%s` · measured on the performance runner.</sub>\n' \
    "${HEAD_SHA:0:12}" "$base_sha"
} > "$directory/comment.md"

existing="$(gh api "repos/$GITHUB_REPOSITORY/issues/$PR_NUMBER/comments" --paginate \
  --jq ".[] | select(.body | contains(\"$MARKER\")) | .id" | tail -n1)"
if [[ -n $existing ]]; then
  gh api --method PATCH "repos/$GITHUB_REPOSITORY/issues/comments/$existing" \
    --field body="$(cat "$directory/comment.md")" >/dev/null
else
  gh api --method POST "repos/$GITHUB_REPOSITORY/issues/$PR_NUMBER/comments" \
    --field body="$(cat "$directory/comment.md")" >/dev/null
fi

if [[ $status -ne 0 ]]; then
  echo "::error::$title"
  exit 1
fi
