param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
if (-not $SkipBuild) {
    $sysroot = & rustc --print sysroot
    if ($LASTEXITCODE -ne 0) { throw 'rustc was not found.' }
    $mingw = Join-Path $sysroot 'lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained'
    if (Test-Path -LiteralPath $mingw) { $env:PATH = "$mingw;$env:PATH" }
    $flags = @(
        '-C', 'target-feature=+crt-static',
        '-C', 'control-flow-guard=checks',
        '-C', 'link-arg=/DEPENDENTLOADFLAG:0x800',
        '-C', 'link-arg=/CETCOMPAT',
        "--remap-path-prefix=$($env:USERPROFILE)=/user",
        "--remap-path-prefix=$PSScriptRoot=/src"
    )
    $env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
    cargo test --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust tests failed.' }
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed.' }
}
$targetDir = Join-Path $PSScriptRoot 'target'
try {
    $meta = (& cargo metadata --format-version 1 --no-deps | ConvertFrom-Json)
    if ($LASTEXITCODE -eq 0 -and -not [string]::IsNullOrWhiteSpace($meta.target_directory)) {
        $targetDir = $meta.target_directory
    }
} catch {}
$releaseDir = Join-Path $targetDir 'release'
$bundle = Join-Path $PSScriptRoot 'dist\TransparentBar'
New-Item -ItemType Directory -Path $bundle -Force | Out-Null
Get-ChildItem -LiteralPath $bundle -Filter 'transparentbar_bridge-*.dll' -File | Remove-Item
$files = (Join-Path $releaseDir 'transparentbar.exe'), 'LICENSE.md', 'README.md'
Copy-Item -LiteralPath $files -Destination $bundle -Force
$commit = git rev-parse HEAD
if ($LASTEXITCODE -ne 0) { throw 'Build must be run from a Git checkout.' }
$version = (Select-String -LiteralPath 'Cargo.toml' -Pattern '^version = "([^"]+)"$' | Select-Object -First 1).Matches.Groups[1].Value
@("Version: $version", "Commit: $commit") | Set-Content -LiteralPath (Join-Path $bundle 'BUILD.txt') -Encoding ascii
$bridgeDll = "transparentbar_bridge-$version.dll"
Copy-Item -LiteralPath (Join-Path $releaseDir 'transparentbar_bridge.dll') -Destination (Join-Path $bundle $bridgeDll) -Force
$hashes = Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $bundle 'transparentbar.exe'), (Join-Path $bundle $bridgeDll)
$hashes | ForEach-Object { '{0}  {1}' -f $_.Hash.ToLowerInvariant(), (Split-Path -Leaf $_.Path) } | Set-Content -LiteralPath (Join-Path $bundle 'SHA256SUMS.txt') -Encoding ascii
$zipName = "TransparentBar-windows-x64-v$version.zip"
Compress-Archive -LiteralPath $bundle -DestinationPath (Join-Path $PSScriptRoot "dist\$zipName") -Force
Get-Item -LiteralPath (Join-Path $bundle 'transparentbar.exe'), (Join-Path $bundle "transparentbar_bridge-$version.dll"), (Join-Path $PSScriptRoot "dist\$zipName") | Select-Object Name, Length
