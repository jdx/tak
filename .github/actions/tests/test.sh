#!/usr/bin/env bash
set -euo pipefail

actions_dir=$(cd "$(dirname "$0")/.." && pwd)
test_dir=$(mktemp -d)

if [[ $(uname -s) == Linux && $(uname -m) == x86_64 ]]; then
  output="$test_dir/install-output"
  RUNNER_OS=Linux \
    RUNNER_ARCH=X64 \
    RUNNER_TEMP="$test_dir" \
    GITHUB_OUTPUT="$output" \
    TAK_VERSION=v0.0.8 \
    TAK_SHA256=442c866a7572936f639c39edb32042a2f60d78a9dd86116a429f6e31cc9840fe \
    "$actions_dir/publish-artifact/install.sh"
  tak_bin=$(cut -d= -f2- "$output")
  [[ -x $tak_bin ]]
  "$tak_bin" --version | grep -q 'tak 0.0.8'
fi

fake_bin="$test_dir/bin"
artifact_dir="$test_dir/artifact"
mkdir -p "$fake_bin" "$artifact_dir"
printf '{}\n' > "$artifact_dir/measurement.json"
cat > "$fake_bin/tak" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" > "$TAK_CAPTURE/args"
printf '%s\n' "$GIT_CONFIG_COUNT" "$GIT_CONFIG_KEY_0" "$GIT_CONFIG_VALUE_0" \
  > "$TAK_CAPTURE/git-config"
EOF
chmod 0755 "$fake_bin/tak"

ARTIFACT="$artifact_dir/measurement.json" \
  EXPECT_REV=0123456789abcdef0123456789abcdef01234567 \
  GH_TOKEN=test-token \
  REMOTE=origin \
  TAK_BIN="$fake_bin/tak" \
  TAK_CAPTURE="$test_dir" \
  "$actions_dir/publish-artifact/publish.sh"
grep -q '^artifact publish .*measurement.json --expect 0123456789abcdef0123456789abcdef01234567 --remote origin$' \
  "$test_dir/args"
grep -q '^http.https://github.com/.extraheader$' "$test_dir/git-config"

cat > "$fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$GH_CAPTURE/calls"
case "$*" in
  *check-runs*) cat > "$GH_CAPTURE/check.json" ;;
  *'comments --paginate'*) ;;
  *issues/*/comments*) printf '%s\n' "$*" > "$GH_CAPTURE/comment-call" ;;
  *) echo "unexpected gh invocation: $*" >&2; exit 1 ;;
esac
EOF
chmod 0755 "$fake_bin/gh"

head_sha=0123456789abcdef0123456789abcdef01234567
printf 'No instruction-count regression.\n' > "$artifact_dir/report.md"
printf '0\n' > "$artifact_dir/status"
printf '0123456789ab fedcba987654\n' > "$artifact_dir/shas"

PATH="$fake_bin:$PATH" \
  GH_CAPTURE="$test_dir" \
  GH_TOKEN=test-token \
  GITHUB_REPOSITORY=jdx/example \
  GITHUB_RUN_ID=123 \
  ARTIFACT_DIRECTORY="$artifact_dir" \
  HEAD_SHA="$head_sha" \
  PR_NUMBER=42 \
  MARKER='<!--example-perf-pr-->' \
  "$actions_dir/report-pr/report.sh"

jq -e --arg sha "$head_sha" \
  '.head_sha == $sha and .conclusion == "success" and .status == "completed"' \
  "$test_dir/check.json" >/dev/null
grep -q 'issues/42/comments' "$test_dir/comment-call"
grep -q '<!--example-perf-pr-->' "$artifact_dir/comment.md"

printf 'ffffffffffff fedcba987654\n' > "$artifact_dir/shas"
calls_before=$(wc -l < "$test_dir/calls")
if PATH="$fake_bin:$PATH" \
  GH_CAPTURE="$test_dir" \
  GH_TOKEN=test-token \
  GITHUB_REPOSITORY=jdx/example \
  GITHUB_RUN_ID=123 \
  ARTIFACT_DIRECTORY="$artifact_dir" \
  HEAD_SHA="$head_sha" \
  PR_NUMBER=42 \
  MARKER='<!--example-perf-pr-->' \
  "$actions_dir/report-pr/report.sh"; then
  echo 'report action accepted an artifact for a different head SHA' >&2
  exit 1
fi
[[ $(wc -l < "$test_dir/calls") -eq $calls_before ]]
