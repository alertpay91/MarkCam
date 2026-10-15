# Play a recorded clip through MarkCam (test mode: off-screen, no virtual camera, no webcam)
# with a virtual background, and save every 15th composited frame to clips\out-<Name>\f#####.png.
#   powershell -ExecutionPolicy Bypass -File tools\vbg-eval\eval.ps1 -Name base
#   ... -Name hq -Extra "OVC_VBG_WIDTH=1280;OVC_VBG_RATIO=0.25"
# Tuning variables read by MarkCam: OVC_VBG_RVM (model file), OVC_VBG_WIDTH (matte width,
# default 640), OVC_VBG_RATIO (RVM downsample ratio, 0.5), OVC_VBG_SHAPE ("lo,hi" alpha
# contrast, 0.12,0.88), OVC_SEG_PROFILE=1 (per-stage timings on stderr).
param(
    [string]$Name,
    [string]$Exe = "$PSScriptRoot\..\..\target\release\markcam.exe",
    [string]$Background = 'builtin:1',
    [string]$Clip = "$PSScriptRoot\clips\live.mp4",
    [int]$Seconds = 34,
    [string]$Extra = '',
    # 0 auto, 1 best, 2 light
    [int]$Quality = 0
)
$out = "$PSScriptRoot\clips\out-$Name"
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
$null = New-Item -ItemType Directory $out
$settings = "$PSScriptRoot\clips\settings-eval.json"
@{ background = $Background; background_quality = $Quality; vcam_on = $false; tab = 'background' } | ConvertTo-Json | Out-File -Encoding ascii $settings
$psi = New-Object Diagnostics.ProcessStartInfo -ArgumentList (Resolve-Path $Exe).Path
$psi.UseShellExecute = $false
$envs = @{ OVC_TEST = '1'; OVC_SETTINGS = $settings; OVC_WINDOW_POS = '-3000,40'; OVC_TEST_MEDIA = (Resolve-Path $Clip).Path; OVC_DUMP = $out }
foreach ($pair in ($Extra -split ';' | Where-Object { $_ -match '=' })) { $k, $v = $pair -split '=', 2; $envs[$k.Trim()] = $v.Trim() }
foreach ($kv in $envs.GetEnumerator()) { $psi.EnvironmentVariables[$kv.Key] = $kv.Value }
$p = [Diagnostics.Process]::Start($psi)
Start-Sleep $Seconds
$p.Kill()
"{0} frames saved in {1}" -f (Get-ChildItem $out -Filter *.png).Count, $out
