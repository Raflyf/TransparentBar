param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
if (-not $SkipBuild) {
    $sysroot = & rustc --print sysroot
    if ($LASTEXITCODE -ne 0) { throw 'rustc tidak ditemukan.' }
    $mingw = Join-Path $sysroot 'lib\rustlib\x86_64-pc-windows-gnu\bin\self-contained'
    if (Test-Path -LiteralPath $mingw) { $env:PATH = "$mingw;$env:PATH" }
    cargo test --locked
    if ($LASTEXITCODE -ne 0) { throw 'Tes Rust gagal.' }
    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Build Rust gagal.' }
}
$bundle = Join-Path $PSScriptRoot 'dist\TransparentBar'
New-Item -ItemType Directory -Path $bundle -Force | Out-Null
$files = 'target\release\transparentbar.exe', 'LICENSE.md', 'README.md'
Copy-Item -LiteralPath $files -Destination $bundle
Copy-Item -LiteralPath 'target\release\transparentbar_bridge.dll' -Destination (Join-Path $bundle 'transparentbar_bridge-0.1.1.dll')
$hashes = Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $bundle 'transparentbar.exe'), (Join-Path $bundle 'transparentbar_bridge-0.1.1.dll')
$hashes | ForEach-Object { '{0}  {1}' -f $_.Hash.ToLowerInvariant(), (Split-Path -Leaf $_.Path) } | Set-Content -LiteralPath (Join-Path $bundle 'SHA256SUMS.txt') -Encoding ascii
Compress-Archive -LiteralPath $bundle -DestinationPath (Join-Path $PSScriptRoot 'dist\TransparentBar-windows-x64.zip') -Force
Get-Item -LiteralPath (Join-Path $bundle 'transparentbar.exe'), (Join-Path $bundle 'transparentbar_bridge-0.1.1.dll'), (Join-Path $PSScriptRoot 'dist\TransparentBar-windows-x64.zip') | Select-Object Name, Length
