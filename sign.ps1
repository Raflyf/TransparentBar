param([Parameter(Mandatory=$true)][string[]]$Files)
$ErrorActionPreference = 'Stop'
if (-not $env:WINDOWS_CERTIFICATE_BASE64) {
    Write-Output 'Signing dilewati: sertifikat code-signing resmi belum tersedia.'
    return
}
if (-not $env:WINDOWS_CERTIFICATE_PASSWORD) { throw 'Password sertifikat signing belum tersedia.' }
$programFiles = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
$tool = Get-ChildItem -Path "$programFiles\Windows Kits\10\bin\*\x64\signtool.exe" | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $tool) { throw 'Windows SDK signtool tidak ditemukan.' }
$certificatePath = Join-Path $env:RUNNER_TEMP 'transparentbar-codesign.pfx'
[IO.File]::WriteAllBytes($certificatePath, [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE_BASE64))
foreach ($file in $Files) {
    & $tool.FullName sign /fd SHA256 /f $certificatePath /p $env:WINDOWS_CERTIFICATE_PASSWORD /tr 'http://timestamp.digicert.com' /td SHA256 $file
    if ($LASTEXITCODE -ne 0) { throw "Signing gagal: $file" }
    & $tool.FullName verify /pa $file
    if ($LASTEXITCODE -ne 0) { throw "Sertifikat tidak terpercaya atau signature invalid: $file" }
    if ((Get-AuthenticodeSignature -LiteralPath $file).Status -ne 'Valid') { throw "Signature tidak valid: $file" }
}
