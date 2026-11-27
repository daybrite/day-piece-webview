# Copyright © The Daybrite Project
# SPDX-License-Identifier: MPL-2.0
# Return a directory containing clang and libclang for mozangle's bindgen.
param([string]$ToolsDirectory = $env:RUNNER_TEMP)
$ErrorActionPreference = 'Stop'

function Test-ServoLlvm([string]$Bin) {
    if (!(Test-Path "$Bin/clang-cl.exe") -or !(Test-Path "$Bin/clang.exe") -or !(Test-Path "$Bin/libclang.dll")) { return $false }
    $version = & "$Bin/clang.exe" --version | Select-Object -First 1
    return $version -match 'clang version (\d+)\.' -and [int]$Matches[1] -ge 22
}

# Visual Studio 18's STL requires Clang 22. Check both toolchains on the hosted image.
foreach ($bin in @("$env:ProgramFiles/LLVM/bin", "$env:VCINSTALLDIR/Tools/Llvm/x64/bin")) {
    if (Test-ServoLlvm $bin) { return $bin }
}

# Bindgen needs libclang and its resource headers. Keep the compiler drivers and
# companion link/archive tools together without installing LLVM system-wide.
$version = '22.1.8'
$name = "LLVM-$version-win64.exe"
$archive = Join-Path $ToolsDirectory $name
$url = "https://github.com/llvm/llvm-project/releases/download/llvmorg-$version/$name"
$checksum = '16e5709785fef73c854646241c4a92c5cd574318d1b33c63330dd7721903e55c'
if (!(Test-Path $archive) -or (Get-FileHash $archive -Algorithm SHA256).Hash -ne $checksum) {
    curl.exe --fail --location --silent --show-error --retry 3 --output "$archive.partial" $url
    if ($LASTEXITCODE -ne 0) { throw "Could not download $url" }
    if ((Get-FileHash "$archive.partial" -Algorithm SHA256).Hash -ne $checksum) { throw "Checksum mismatch for $name" }
    Move-Item -Force "$archive.partial" $archive
}
$root = Join-Path $ToolsDirectory "servo-llvm-$version"
$components = @('bin/clang.exe', 'bin/clang++.exe', 'bin/clang-cl.exe', 'bin/libclang.dll',
                'bin/lld-link.exe', 'bin/llvm-ar.exe', 'bin/llvm-lib.exe', 'bin/llvm-ranlib.exe', 'lib/clang')
& 7z.exe x -y "-o$root" $archive @components | Out-Host
if ($LASTEXITCODE -ne 0 -or !(Test-ServoLlvm "$root/bin")) { throw "Could not extract clang/libclang from $name" }
return "$root/bin"
