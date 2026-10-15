# Record a test clip from the webcam for virtual-background tuning (MarkCam and ClassIn must
# be closed so the camera is free). Sit still ~10 s, wave / spread fingers ~10 s, turn the
# head ~10 s.
#   powershell -ExecutionPolicy Bypass -File tools\vbg-eval\record.ps1 [-Seconds 30] [-Camera "HD Pro Webcam C920"]
param([int]$Seconds = 30, [string]$Camera = 'HD Pro Webcam C920', [string]$Out = "$PSScriptRoot\clips\live.mp4")
$ffmpeg = "$env:APPDATA\MarkCam\tools\ffmpeg.exe"
if (-not (Test-Path $ffmpeg)) { $ffmpeg = 'C:\Program Files\MarkCam\tools\ffmpeg.exe' }
$null = New-Item -ItemType Directory -Force (Split-Path $Out)
"Recording $Seconds s from '$Camera' in 5 s..."
Start-Sleep 5
& $ffmpeg -hide_banner -v error -y -f dshow -rtbufsize 200M -video_size 1280x720 -framerate 30 -vcodec mjpeg -i "video=$Camera" -t $Seconds -c:v libx264 -crf 14 -preset fast $Out
"saved $Out"
