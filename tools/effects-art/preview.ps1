# Render the art, install it for development (%APPDATA%\MarkCam\Packs) and screenshot each
# face effect of the given categories on the recorded clip, into one contact sheet per
# category (assets\out\preview-<category>.png).
#   powershell -ExecutionPolicy Bypass -File tools\effects-art\preview.ps1 -Categories "01 Hats","02 Hair"
param([string]$Categories, [string]$Clip = "$PSScriptRoot\..\vbg-eval\clips\live.mp4", [int]$Parallel = 4, [switch]$NoRender)
$root = Resolve-Path "$PSScriptRoot\..\.."
$out = "$root\assets\out"
if (-not $NoRender) { & "$PSScriptRoot\target\release\effects-art.exe" "$root\assets" $out "$env:APPDATA\MarkCam\tools\ffmpeg.exe" }
$packs = "$env:APPDATA\MarkCam\Packs"
if (Test-Path $packs) { Remove-Item $packs -Recurse -Force }
Copy-Item "$out\effects" $packs -Recurse
if (Test-Path "$out\backgrounds") {
    $bg = "$env:APPDATA\MarkCam\PackBackgrounds"
    if (Test-Path $bg) { Remove-Item $bg -Recurse -Force }
    Copy-Item "$out\backgrounds" $bg -Recurse
}
$exe = "$root\target\release\markcam.exe"
$tmp = "$out\preview-tmp"
if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
$null = New-Item -ItemType Directory $tmp
Add-Type -AssemblyName System.Drawing
foreach ($cat in ($Categories -split ',' | ForEach-Object { $_.Trim() } | Where-Object { $_ })) {
    $pngs = Get-ChildItem "$packs\$cat" -File | Where-Object { $_.Extension -in '.png', '.webp' }
    $jobs = @()
    $shots = @()
    foreach ($p in $pngs) {
        $shot = "$tmp\$($p.BaseName).png"
        $settings = "$tmp\$($p.BaseName).json"
        $layer = @{ source = @{ Image = $p.FullName }; pos = @(0.0, 0.0); size = @(1.0, 1.0); scale = 1.0; opacity = 1.0; on = $true; remove_green = $false }
        @{ background = 'off'; vcam_on = $false; tab = 'layers'; layers = @($layer) } | ConvertTo-Json -Depth 6 | Out-File -Encoding ascii $settings
        $psi = New-Object Diagnostics.ProcessStartInfo -ArgumentList $exe
        $psi.UseShellExecute = $false
        foreach ($kv in @{ OVC_TEST = '1'; OVC_SETTINGS = $settings; OVC_WINDOW_POS = '-3000,40'; OVC_SCREENSHOT = $shot; OVC_TEST_MEDIA = (Resolve-Path $Clip).Path }.GetEnumerator()) { $psi.EnvironmentVariables[$kv.Key] = $kv.Value }
        $jobs += [Diagnostics.Process]::Start($psi)
        $shots += $shot
        if ($jobs.Count -ge $Parallel) { foreach ($j in $jobs) { $null = $j.WaitForExit(60000) }; $jobs = @() }
    }
    foreach ($j in $jobs) { $null = $j.WaitForExit(60000) }
    # Sheet: the preview area of each screenshot, 4 per row.
    $shots = $shots | Where-Object { Test-Path $_ }
    $n = @($shots).Count
    if ($n -eq 0) { continue }
    $tw = 380; $th = 214; $cols = 4; $rows = [math]::Ceiling($n / $cols)
    $sheet = New-Object Drawing.Bitmap ($tw * $cols), ($th * $rows)
    $g = [Drawing.Graphics]::FromImage($sheet)
    $g.InterpolationMode = 'HighQualityBicubic'
    $i = 0
    foreach ($s in $shots) {
        $img = [Drawing.Bitmap]::FromFile($s)
        # Crop around the face area of the preview.
        $src = New-Object Drawing.Rectangle 150, 160, 440, 248
        $dst = New-Object Drawing.Rectangle (($i % $cols) * $tw), ([math]::Floor($i / $cols) * $th), $tw, $th
        $g.DrawImage($img, $dst, $src, [Drawing.GraphicsUnit]::Pixel)
        $g.DrawString([IO.Path]::GetFileNameWithoutExtension($s), (New-Object Drawing.Font 'Arial', 10), [Drawing.Brushes]::Yellow, $dst.X + 3, $dst.Y + 3)
        $img.Dispose(); $i++
    }
    $sheet.Save("$out\preview-$cat.png"); $g.Dispose(); $sheet.Dispose()
    "preview: $out\preview-$cat.png ($n)"
}
