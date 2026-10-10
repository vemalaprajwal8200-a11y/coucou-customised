param(
  [Parameter(Mandatory = $true)]
  [string]$CertificateThumbprint
)

$ErrorActionPreference = "Stop"
$windowsRoot = Split-Path -Parent $PSScriptRoot
$repoRoot = Split-Path -Parent $windowsRoot
$package = Get-Content (Join-Path $windowsRoot "package.json") -Raw | ConvertFrom-Json
$version = $package.version
$stage = Join-Path $windowsRoot "target\msix\stage"
$output = Join-Path $windowsRoot "target\msix\Coucou-Windows-$version-x64.msix"

function Find-SdkTool([string]$name) {
  $onPath = Get-Command "$name.exe" -ErrorAction SilentlyContinue
  if ($onPath) { return $onPath.Source }
  $sdkRoot = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
  if (Test-Path $sdkRoot) {
    $versions = Get-ChildItem $sdkRoot -Directory | Sort-Object Name -Descending
    foreach ($versionDirectory in $versions) {
      $candidate = Join-Path $versionDirectory.FullName "x64\$name.exe"
      if (Test-Path $candidate) { return $candidate }
    }
  }
  throw "$name.exe was not found. Install the Windows 10/11 SDK and retry."
}

$thumbprint = $CertificateThumbprint.Replace(" ", "").ToUpperInvariant()
$certificate = Get-ChildItem "Cert:\CurrentUser\My\$thumbprint" -ErrorAction SilentlyContinue
if (!$certificate -or !$certificate.HasPrivateKey) {
  throw "A signing certificate with a private key and thumbprint $thumbprint is required in CurrentUser\My."
}

$makeAppx = Find-SdkTool "MakeAppx"
$signTool = Find-SdkTool "SignTool"

Push-Location $windowsRoot
try {
  npm run build
  if ($LASTEXITCODE -ne 0) { throw "Frontend build failed." }
  cargo build --release --workspace
  if ($LASTEXITCODE -ne 0) { throw "Rust release build failed." }
  npx tauri build --no-bundle
  if ($LASTEXITCODE -ne 0) { throw "Tauri app build failed." }
} finally {
  Pop-Location
}

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path (Join-Path $stage "Assets") -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $stage "resources") -Force | Out-Null

$release = Join-Path $windowsRoot "target\release"
Copy-Item (Join-Path $release "coucou.exe") (Join-Path $stage "Coucou.exe")
Copy-Item (Join-Path $release "coucou-hook.exe") (Join-Path $stage "resources\coucou-hook.exe")
Copy-Item (Join-Path $repoRoot "stt_server.py") (Join-Path $stage "resources\stt_server.py")
$iconRoot = Join-Path $windowsRoot "src-tauri\icons"
Copy-Item (Join-Path $iconRoot "50x50.png") (Join-Path $stage "Assets\StoreLogo.png")
Copy-Item (Join-Path $iconRoot "150x150.png") (Join-Path $stage "Assets\Square150x150Logo.png")
Copy-Item (Join-Path $iconRoot "44x44.png") (Join-Path $stage "Assets\Square44x44Logo.png")

$manifestPath = Join-Path $windowsRoot "src-tauri\msix\AppxManifest.xml"
$manifest = Get-Content $manifestPath -Raw
$publisher = [System.Security.SecurityElement]::Escape($certificate.Subject)
$manifest = $manifest.Replace("__PUBLISHER__", $publisher)
$manifest = $manifest.Replace('Version="0.1.1.0"', "Version=`"$version.0`"")
Set-Content (Join-Path $stage "AppxManifest.xml") $manifest -Encoding UTF8

New-Item -ItemType Directory -Path (Split-Path $output -Parent) -Force | Out-Null
if (Test-Path $output) { Remove-Item $output -Force }
& $makeAppx pack /d $stage /p $output /o
if ($LASTEXITCODE -ne 0) { throw "MakeAppx failed to package the application." }
& $signTool sign /fd SHA256 /sha1 $thumbprint $output
if ($LASTEXITCODE -ne 0) { throw "SignTool could not sign the MSIX package." }
& $signTool verify /pa /v $output
if ($LASTEXITCODE -ne 0) { throw "The MSIX signature could not be verified." }

Write-Host "Signed package created: $output"
