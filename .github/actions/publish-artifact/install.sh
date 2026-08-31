#!/usr/bin/env bash
set -euo pipefail

[[ ${RUNNER_OS:-} == Linux && ${RUNNER_ARCH:-} == X64 ]] || {
  echo "::error::publish-artifact currently supports only Linux X64 hosted runners"
  exit 1
}
[[ ${TAK_VERSION:-} =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "::error::version must be an exact stable Tak tag, such as v0.0.9"
  exit 1
}
[[ ${TAK_SHA256:-} =~ ^[0-9a-f]{64}$ ]] || {
  echo "::error::sha256 must be 64 lowercase hexadecimal characters"
  exit 1
}

install_dir="${RUNNER_TEMP:?}/tak-${TAK_VERSION}-${TAK_SHA256}"
archive="$install_dir/tak.tar.gz"
mkdir -p "$install_dir"

if ! printf '%s  %s\n' "$TAK_SHA256" "$archive" | sha256sum --check --status 2>/dev/null; then
  download="$install_dir/tak.tar.gz.download"
  curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    "https://github.com/jdx/tak/releases/download/${TAK_VERSION}/tak-x86_64-unknown-linux-musl.tar.gz" \
    --output "$download"
  printf '%s  %s\n' "$TAK_SHA256" "$download" | sha256sum --check --status
  mv "$download" "$archive"
fi

tar -xzf "$archive" -C "$install_dir" tak
chmod 0755 "$install_dir/tak"
echo "bin=$install_dir/tak" >> "${GITHUB_OUTPUT:?}"
