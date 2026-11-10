param([switch]$Release)

$ErrorActionPreference = "Stop"

if ($Release) { cargo build --release } else { cargo build }
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

cargo test
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$exe = if ($Release) { "target\release\zap.exe" } else { "target\debug\zap.exe" }
& $exe --version
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host ""
Write-Host "built: $exe"
