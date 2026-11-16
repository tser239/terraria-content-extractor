# Build the portable Windows release zip.
#
# Usage (from the repository root or anywhere):
#   pwsh -File scripts/package-windows.ps1
#   pwsh -File scripts/package-windows.ps1 -NoStaticCrt
#
# The default build statically links the MSVC runtime, so the resulting exe is a
# single self-contained file that does not need the VC++ Redistributable.
param(
    [switch]$NoStaticCrt
)

$ErrorActionPreference = "Stop"

$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

$versionMatch = Select-String -Path "Cargo.toml" -Pattern '^version = "(.*)"'
if (-not $versionMatch) {
    throw "could not read the package version from Cargo.toml"
}
$Version = $versionMatch.Matches[0].Groups[1].Value

$Dist = Join-Path $Root "dist"
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

$Target = "x86_64-pc-windows-msvc"
$Exe = Join-Path $Root "target/$Target/release/terraria-content-extractor.exe"
$Zip = Join-Path $Dist "terraria-content-extractor-$Version-x86_64-windows.zip"
$Checksums = Join-Path $Dist "SHA256SUMS-windows-x86_64"

if (Get-Command rustup -ErrorAction SilentlyContinue) {
    rustup target add $Target | Out-Null
}

if (-not $NoStaticCrt) {
    $env:RUSTFLAGS = ("$env:RUSTFLAGS -C target-feature=+crt-static").Trim()
}

Write-Host "==> cargo build --release"
cargo build --release --locked --target $Target --bin terraria-content-extractor
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed"
}

$Stage = Join-Path $Dist "stage-windows"
Remove-Item -Recurse -Force $Stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $Stage | Out-Null
Copy-Item $Exe, (Join-Path $Root "LICENSE"), (Join-Path $Root "README.md") -Destination $Stage

Remove-Item -Force $Zip -ErrorAction SilentlyContinue
Compress-Archive -Path (Join-Path $Stage "*") -DestinationPath $Zip -CompressionLevel Optimal
Remove-Item -Recurse -Force $Stage

# A static CRT should leave no VCRUNTIME/MSVCP imports behind.
$dumpbin = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
if ($null -ne $dumpbin) {
    $deps = & $dumpbin.Source /nologo /dependents $Exe
    if ($deps -match "VCRUNTIME|MSVCP") {
        Write-Warning "the executable still imports the dynamic C++ runtime:"
        $deps | Select-String "VCRUNTIME|MSVCP"
    }
} else {
    Write-Host "dumpbin not found; skipping the runtime dependency check"
}

$hash = (Get-FileHash $Zip -Algorithm SHA256).Hash.ToLower()
"$hash  $(Split-Path -Leaf $Zip)" | Set-Content -Encoding ascii $Checksums

Write-Host ""
Write-Host "Created in $Dist:"
Write-Host "  $(Split-Path -Leaf $Zip)"
Write-Host "  $(Split-Path -Leaf $Checksums)"
