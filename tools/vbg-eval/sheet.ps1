# Tile chosen frames of an eval run (3 per row, 640 px each) into clips\sheet-<Name>.png for
# side-by-side review. -Crop x,y,w,h zooms into that region of every frame.
#   powershell -ExecutionPolicy Bypass -File tools\vbg-eval\sheet.ps1 -Name base -Frames 330,345,510 -Crop 40,0,1240,440
param([string]$Name, [string]$Frames = '90,330,420,510,660,810', [string]$Crop = '')
$frameList = @($Frames -split '[, ]+' | Where-Object { $_ } | ForEach-Object { [int]$_ })
$cropBox = @($Crop -split '[, ]+' | Where-Object { $_ } | ForEach-Object { [int]$_ })
Add-Type -AssemblyName System.Drawing
$dir = "$PSScriptRoot\clips\out-$Name"
$files = $frameList | ForEach-Object { Join-Path $dir ("f{0:D5}.png" -f $_) } | Where-Object { Test-Path $_ }
$src = if ($cropBox.Count -eq 4) { New-Object Drawing.Rectangle $cropBox[0], $cropBox[1], $cropBox[2], $cropBox[3] } else { New-Object Drawing.Rectangle 0, 0, 1280, 720 }
$tw = 640; $th = [int]($src.Height * 640 / $src.Width)
$n = @($files).Count; $rows = [math]::Ceiling($n / 3)
$sheet = New-Object Drawing.Bitmap ($tw * [math]::Min(3, $n)), ($th * $rows)
$g = [Drawing.Graphics]::FromImage($sheet)
$g.InterpolationMode = 'HighQualityBicubic'
$i = 0
foreach ($f in $files) {
    $img = [Drawing.Bitmap]::FromFile($f)
    $dst = New-Object Drawing.Rectangle (($i % 3) * $tw), ([math]::Floor($i / 3) * $th), $tw, $th
    $g.DrawImage($img, $dst, $src, [Drawing.GraphicsUnit]::Pixel)
    $g.DrawString((Split-Path $f -Leaf), (New-Object Drawing.Font 'Arial', 11), [Drawing.Brushes]::Yellow, $dst.X + 4, $dst.Y + 4)
    $img.Dispose(); $i++
}
$sheet.Save("$PSScriptRoot\clips\sheet-$Name.png"); $g.Dispose(); $sheet.Dispose()
"sheet: $n frames -> $PSScriptRoot\clips\sheet-$Name.png"
