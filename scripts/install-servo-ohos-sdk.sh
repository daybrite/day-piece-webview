#!/usr/bin/env bash
# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
# Install only Servo's API-20 native compiler, separate from Day's packaging SDK.
set -euo pipefail
: "${RUNNER_TEMP:?This installer requires a CI temporary directory}"
: "${GITHUB_ENV:?This installer requires the CI environment file}"
sdk_dir="$RUNNER_TEMP/servo-ohos-sdk-6.0.0.1"
archive=ohos-sdk-windows_linux-public.tar.gz
mkdir -p "$sdk_dir"
cd "$sdk_dir"
if [[ ! -x "$sdk_dir/native/llvm/bin/clang" ]]; then
  gh release download v6.0.0.1 --repo openharmony-rs/ohos-sdk \
    --pattern "$archive.*" --clobber
  cat "$archive.aa" "$archive.ab" > "$archive"
  # Pin the release digest as well as checking the downloaded checksum sidecar.
  digest=42955c8106cab9349a5eabfdc055543b9e49111a4ea68705f7a54bdf65c7834a
  test "$(tr -d '[:space:]' < "$archive.sha256")" = "$digest"
  echo "$digest  $archive" | sha256sum -c -
  mkdir -p unpacked
  component="$(tar -tf "$archive" | awk -F / 'NF == 3 && $2 == "linux" && $3 ~ /^native-.*[.]zip$/ { print }')"
  if [[ -z "$component" || "$component" == *$'\n'* ]]; then
    echo "Expected exactly one Linux native SDK archive member" >&2
    exit 1
  fi
  tar -xf "$archive" -C unpacked --strip-components=1 "$component"
  shopt -s nullglob
  components=(unpacked/linux/native-*.zip)
  if [[ ${#components[@]} != 1 ]]; then
    echo "Expected exactly one Linux native SDK component" >&2
    exit 1
  fi
  unzip -q -o "${components[0]}" -d "$sdk_dir"
  test -x "$sdk_dir/native/llvm/bin/clang"
  rm "$archive" "$archive.aa" "$archive.ab" "$archive.sha256" "${components[0]}"
fi
{
  echo "OHOS_SDK_NATIVE=$sdk_dir/native"
  echo "OHOS_NDK_HOME=$sdk_dir/native"
} >> "$GITHUB_ENV"
# OHOS_BASE_SDK_HOME, hvigor, hdc and the API-18 packaging tools remain unchanged.
