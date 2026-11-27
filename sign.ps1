param(
    [Parameter(Mandatory = $true)][string[]]$Files,
    [switch]$RequireCertificate
)
$ErrorActionPreference = 'Stop'
$mustSign = $RequireCertificate.IsPresent -or ($env:REQUIRE_SIGNING -in '1', 'true', 'True', 'TRUE')
if (-not $env:WINDOWS_CERTIFICATE_BASE64) {
    if ($mustSign) {
        throw 'Signing required, but WINDOWS_CERTIFICATE_BASE64 is not configured.'
    }
    Write-Output 'Signing skipped: code-signing certificate is not configured.'
    return
}
if (-not $env:WINDOWS_CERTIFICATE_PASSWORD) {
    throw 'WINDOWS_CERTIFICATE_PASSWORD is not configured.'
}
$programFiles = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
$tool = Get-ChildItem -Path "$programFiles\Windows Kits\10\bin\*\x64\signtool.exe" | Sort-Object FullName -Descending | Select-Object -First 1
if (-not $tool) {
    throw 'Windows SDK signtool.exe was not found.'
}
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [IO.Path]::GetTempPath() }
$certificatePath = Join-Path $tempRoot ("transparentbar-codesign-{0}.pfx" -f [guid]::NewGuid().ToString('N'))
$pfxBytes = [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE_BASE64)
try {
    [IO.File]::WriteAllBytes($certificatePath, $pfxBytes)
    foreach ($file in $Files) {
        & $tool.FullName sign /fd SHA256 /f $certificatePath /p $env:WINDOWS_CERTIFICATE_PASSWORD /tr 'https://timestamp.digicert.com' /td SHA256 $file
        if ($LASTEXITCODE -ne 0) { throw "Signing failed: $file" }
        & $tool.FullName verify /pa $file
        if ($LASTEXITCODE -ne 0) { throw "Untrusted certificate or invalid signature: $file" }
        if ([IO.Path]::GetExtension($file) -in '.exe', '.dll', '.msi') {
            if ((Get-AuthenticodeSignature -LiteralPath $file).Status -ne 'Valid') {
                throw "Authenticode signature is not valid: $file"
            }
        }
    }
}
finally {
    [Array]::Clear($pfxBytes, 0, $pfxBytes.Length)
    if (Test-Path -LiteralPath $certificatePath) {
        try {
            $len = (Get-Item -LiteralPath $certificatePath).Length
            if ($len -gt 0) {
                [IO.File]::WriteAllBytes($certificatePath, [byte[]]::new($len))
            }
        }
        catch {
        }
        [IO.File]::Delete($certificatePath)
    }
}
