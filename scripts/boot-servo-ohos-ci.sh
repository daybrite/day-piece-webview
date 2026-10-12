#!/usr/bin/env bash
# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
# CI only: the same pinned phone image used by the shared Day application workflow.
set -euo pipefail
: "${RUNNER_TEMP:?CI runner required}" "${GITHUB_ENV:?}" "${DAY_BIN:?}"
archive="$HOME/.cache/ohos-qemu/v20260919/openharmony-qemu-x86_64-x86_64_virt-phone.tar.gz"
if [[ ! -f "$archive" ]]; then
  mkdir -p "$(dirname "$archive")"
  curl -fL -o "$archive" https://github.com/harmony-contrib/ohos-qemu/releases/download/v20260919/openharmony-qemu-x86_64-x86_64_virt-phone.tar.gz
fi
echo "08d35399119ec9b87d564cd8bf024e8a189921f5bacf09da889b7b223848a488  $archive" | sha256sum -c -
image_dir="$(mktemp -d "$RUNNER_TEMP/servo-ohos.XXXXXX")"
tar -xzf "$archive" -C "$image_dir" --warning=no-unknown-keyword
kernel="$(find "$image_dir" -name bzImage -print -quit)"
test -n "$kernel"
DAY_OHOS_EMULATOR="$(dirname "$kernel")"
export DAY_OHOS_EMULATOR
echo "DAY_OHOS_EMULATOR=$DAY_OHOS_EMULATOR" >> "$GITHUB_ENV"
"$DAY_BIN" devices boot -t harmony-arkui --headless --wait --device phone
