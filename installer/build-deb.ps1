# Build dist\markcam_<version>_amd64.deb (Linux Mint / Ubuntu) from Windows, through WSL.
#
#   powershell -ExecutionPolicy Bypass -File installer\build-deb.ps1 -Version 1.3.9
#
# Gathers on Windows what both systems share (models, MarkCam's rendered effects and
# backgrounds, licenses, the source zip) into installer\stage-linux, copies the repository
# into the WSL distro (building on its own disk is far faster than on /mnt/c), and runs
# installer/linux/build-deb.sh there. The Linux-only parts (ONNX Runtime for Linux, yt-dlp)
# are downloaded once into ~/deps inside WSL.
param(
    [string]$Version = '1.0.0',
    [string]$Distro = 'Ubuntu-24.04',
    [string]$OrtVersion = '1.24.4'
)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$stage = Join-Path $PSScriptRoot 'stage-linux'
$data = Join-Path $env:APPDATA 'MarkCam'
function Need($path) { if (-not (Test-Path $path)) { throw "missing: $path" }; $path }

# MarkCam's own effects and backgrounds (rendered from assets\ when not there yet).
if (-not (Test-Path "$root\assets\out\effects")) {
    $art = "$root\tools\effects-art"
    cargo build --release -q --manifest-path "$art\Cargo.toml"
    if ($LASTEXITCODE -ne 0) { throw 'effects-art build failed' }
    & (Need "$art\target\release\effects-art.exe") "$root\assets" "$root\assets\out" "$data\tools\ffmpeg.exe"
    if ($LASTEXITCODE -ne 0) { throw 'rendering the effects failed' }
}

if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
$null = New-Item -ItemType Directory $stage, "$stage\models", "$stage\effects", "$stage\licenses", "$stage\source"
foreach ($m in 'version-RFB-320.onnx', 'facemesh_v2.onnx', 'modnet_webcam.onnx', 'rvm_mobilenetv3_fp32.onnx', 'rvm_resnet50_fp32.onnx', 'selfie_landscape.onnx') { Copy-Item (Need "$data\models\$m") "$stage\models\" }
Copy-Item "$root\assets\out\effects" "$stage\effects\MarkCam" -Recurse
Copy-Item "$root\assets\out\backgrounds" "$stage\backgrounds" -Recurse
foreach ($l in 'MODNet-LICENSE.txt', 'RVM-LICENSE.txt') { Copy-Item (Need "$data\runtime\$l") "$stage\licenses\" }
Copy-Item (Need "$root\assets\EMOJI-LICENSES.txt") "$stage\licenses\"
Copy-Item "$root\crates\ovc-app\assets\fonts\*.txt" "$stage\licenses\"
Copy-Item (Need "$root\LICENSE") "$stage\LICENSE.txt"
Copy-Item (Need "$root\THIRD-PARTY-NOTICES.txt") "$stage\THIRD-PARTY-NOTICES.txt"
$zip = Join-Path $root "dist\MarkCam-$Version-source.zip"
if (Test-Path $zip) { Copy-Item $zip "$stage\source\" }

$wroot = '/mnt/' + $root.Substring(0, 1).ToLower() + ($root.Substring(2) -replace '\\', '/')
$script = @"
set -e
mkdir -p ~/deps ~/markcam
cd ~/deps
[ -d onnxruntime-linux-x64-$OrtVersion ] || { curl -sSLf -o ort.tgz https://github.com/microsoft/onnxruntime/releases/download/v$OrtVersion/onnxruntime-linux-x64-$OrtVersion.tgz && tar xzf ort.tgz && rm ort.tgz; }
[ -x yt-dlp ] || { curl -sSLf -o yt-dlp https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp_linux && chmod +x yt-dlp; }
rsync -a --delete --exclude target --exclude dist --exclude .git --exclude 'installer/stage' --exclude 'installer/stage-linux' --exclude 'assets/out' --exclude 'tools/vbg-eval' --exclude 'tools/effects-art/target' $wroot/ ~/markcam/
bash ~/markcam/installer/linux/build-deb.sh $Version $wroot/installer/stage-linux ~/deps
mkdir -p $wroot/dist && cp ~/markcam/dist/markcam_${Version}_amd64.deb $wroot/dist/
"@
$script = $script -replace "`r", ''
wsl.exe -d $Distro -u root -- bash -lc $script
if ($LASTEXITCODE -ne 0) { throw "the Linux build failed ($LASTEXITCODE)" }
$out = Join-Path $root "dist\markcam_${Version}_amd64.deb"
"built {0} ({1:N0} MB)" -f $out, ((Get-Item $out).Length / 1MB)
