# Build dist\MarkCam-Setup-<version>.exe: release build, gather everything into
# installer\stage, compile installer\markcam.iss with Inno Setup 6.
#
#   powershell -ExecutionPolicy Bypass -File installer\build-installer.ps1 [-Version 1.0.0]
#
# Bundled from this computer: the face / background models, ONNX Runtime and video tools
# (%APPDATA%\MarkCam), the VB-CABLE driver pack, MarkCam's own effects and backgrounds
# (rendered from assets\), and - unless -Public - the user's "My effects" (files over
# -MaxEffectMB are left out) and library.json (categories). Nothing from ManyCam.
param(
    [string]$Version = '1.0.0',
    [int]$MaxEffectMB = 150,
    [switch]$NoBuild,
    # A build to share: without this computer's "My effects" and category layout.
    [switch]$Public
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$stage = Join-Path $PSScriptRoot 'stage'
$data = Join-Path $env:APPDATA 'MarkCam'

$iscc = @("$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) not found' }

if (-not $NoBuild) {
    # NASM is required: without it mozjpeg builds without SIMD and webcam decoding costs
    # ~2x the CPU (it showed up on the laptop).
    $nasm = Get-ChildItem "$env:LOCALAPPDATA\Programs\nasm-*\nasm.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($nasm) { $env:Path += ";$($nasm.DirectoryName)" }
    if (-not (Get-Command nasm -ErrorAction SilentlyContinue)) { throw 'NASM not found (needed for fast MJPEG decoding)' }
    Push-Location $root
    # The public build hides the ManyCam options and keeps "VB-CABLE" in the microphone name.
    if ($Public) { $env:MARKCAM_PUBLIC = '1' } else { Remove-Item Env:MARKCAM_PUBLIC -ErrorAction SilentlyContinue }
    cargo build --release -p ovc-app -p ovc-dshow
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    Pop-Location
}

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
$null = New-Item -ItemType Directory $stage, "$stage\models", "$stage\tools", "$stage\drivers\vbcable", "$stage\effects"

function Need($path) { if (-not (Test-Path $path)) { throw "missing: $path" }; $path }

Copy-Item (Need "$root\target\release\markcam.exe") "$stage\MarkCam.exe"
Copy-Item (Need "$root\target\release\markcam_dshow.dll") "$stage\markcam_dshow.dll"
Copy-Item (Need "$PSScriptRoot\README.txt") "$stage\README.txt"
Copy-Item (Need "$root\LICENSE") "$stage\LICENSE.txt"
Copy-Item (Need "$root\THIRD-PARTY-NOTICES.txt") "$stage\THIRD-PARTY-NOTICES.txt"
foreach ($m in 'version-RFB-320.onnx', 'facemesh_v2.onnx', 'modnet_webcam.onnx', 'rvm_mobilenetv3_fp32.onnx', 'rvm_resnet50_fp32.onnx', 'selfie_landscape.onnx') { Copy-Item (Need "$data\models\$m") "$stage\models\" }
# Virtual background: ONNX Runtime (MIT) + DirectML (Microsoft, redistributable) next to the
# exe, and their licenses (with MODNet's, Apache-2.0) under licenses\.
foreach ($d in 'onnxruntime.dll', 'onnxruntime_providers_shared.dll', 'DirectML.dll') { Copy-Item (Need "$data\runtime\$d") "$stage\" }
$null = New-Item -ItemType Directory "$stage\licenses"
foreach ($l in 'onnxruntime-LICENSE.txt', 'onnxruntime-ThirdPartyNotices.txt', 'DirectML-LICENSE.txt', 'MODNet-LICENSE.txt', 'RVM-LICENSE.txt') { Copy-Item (Need "$data\runtime\$l") "$stage\licenses\" }
# MarkCam's own effects and backgrounds, rendered from assets\ (SVG sources).
$art = "$root\tools\effects-art"
if (-not $NoBuild) {
    cargo build --release -q --manifest-path "$art\Cargo.toml"
    if ($LASTEXITCODE -ne 0) { throw 'effects-art build failed' }
}
& (Need "$art\target\release\effects-art.exe") "$root\assets" "$root\assets\out" "$data\tools\ffmpeg.exe"
if ($LASTEXITCODE -ne 0) { throw 'rendering the effects failed' }
Copy-Item "$root\assets\out\backgrounds" "$stage\backgrounds" -Recurse
Copy-Item (Need "$root\assets\EMOJI-LICENSES.txt") "$stage\licenses\"
# Banner fonts (embedded in MarkCam.exe): their licenses.
Copy-Item "$root\crates\ovc-app\assets\fonts\*.txt" "$stage\licenses\"
# Studio voice (built into MarkCam.exe): RNNoise / nnnoiseless and DeepFilterNet.
Copy-Item "$PSScriptRoot\licenses\*.txt" "$stage\licenses\"
foreach ($t in 'yt-dlp.exe', 'ffmpeg.exe') { Copy-Item (Need "$data\tools\$t") "$stage\tools\" }
# VB-CABLE driver pack (only the files its installer needs).
Copy-Item (Need "$data\tools\vbcable\*") "$stage\drivers\vbcable\" -Recurse
Need "$stage\drivers\vbcable\VBCABLE_Setup_x64.exe" | Out-Null

# Effects library: MarkCam's own (no ManyCam files — ManyCam's can be turned on in Setup
# where it is installed), plus this computer's "My effects" unless -Public.
Copy-Item "$root\assets\out\effects" "$stage\effects\MarkCam" -Recurse
$skipped = @()
if (-not $Public -and (Test-Path "$data\My Effects")) {
    $null = New-Item -ItemType Directory "$stage\effects\My Effects"
    foreach ($f in Get-ChildItem "$data\My Effects" -File) {
        if ($f.Length -gt $MaxEffectMB * 1MB) { $skipped += "$($f.Name) ($([int]($f.Length / 1MB)) MB)"; continue }
        Copy-Item -LiteralPath $f.FullName "$stage\effects\My Effects\"
    }
}
if (-not $Public -and (Test-Path "$data\library.json")) { Copy-Item "$data\library.json" "$stage\effects\library.json" }

# The source code (GPL): a zip of the repository without build outputs, downloads and
# personal files, installed next to MarkCam.exe.
if ($Public) {
    $src = Join-Path $env:TEMP "markcam-source"
    if (Test-Path $src) { Remove-Item $src -Recurse -Force }
    $null = robocopy $root "$src\MarkCam" /E /NFL /NDL /NJH /NJS /XD target dist stage stage-linux out cache clips .vscode .idea .claude preview-tmp /XF LAPTOP-DIJAGNOSTIKA.md *.mp4 *.onnx
    $null = New-Item -ItemType Directory "$stage\source"
    Compress-Archive -Path "$src\MarkCam" -DestinationPath "$stage\source\MarkCam-$Version-source.zip" -CompressionLevel Optimal
    Copy-Item "$stage\source\MarkCam-$Version-source.zip" (Join-Path $root "dist\") -Force
}

$files = Get-ChildItem $stage -Recurse -File
"staged {0} files, {1:N0} MB" -f $files.Count, (($files | Measure-Object Length -Sum).Sum / 1MB)
if ($skipped) { "left out of My effects (too big): $($skipped -join ', ')" }

$suffix = if ($Public) { '-public' } else { '' }
& $iscc "/DAppVersion=$Version" "/DSuffix=$suffix" "/Q" "$PSScriptRoot\markcam.iss"
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
$out = Join-Path $root "dist\MarkCam-Setup-$Version$suffix.exe"
"built {0} ({1:N0} MB)" -f $out, ((Get-Item $out).Length / 1MB)
