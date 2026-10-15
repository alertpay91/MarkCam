# MarkCam — laptop performance check (handoff for Claude on the laptop)

## ROUND 2 (version 1.0.1) — do this now
Round 1 (your report) was right: the UI thread repainted twice per frame (egui paints a
zero-delay repaint request twice), the test pattern was generated per pixel with floats
at 60 fps (that was the busy "worker" thread in the test runs), and the shipped build had
no SIMD for MJPEG decoding (NASM was missing). Fixed in 1.0.1, plus: the pipeline
(effects → virtual camera) now runs on the capture thread, and the window repaints every
frame only while MarkCam is focused, every 3rd frame behind another window, never while
minimized; the virtual camera still gets every frame. Audio mixer wakes every 10 ms (was 5).
Desktop results, ms of CPU per second (old → new): camera visible 294 → 170, camera
minimized 230 → 91, test pattern visible 516 → 110.

1. Install `MarkCam-Setup-1.0.1.exe` (the user copies it from the desktop's
   `openvcam\dist`), over the old one.
2. Repeat the same scenarios as round 1 (a, a2, b, c1, c2), with ClassIn closed.
3. Also run each with a timing log and include its last lines in the report:
   `$env:OVC_PERF = "$env:TEMP\mc-perf.log"` before `Start-Process` — one line per 5 s:
   repaints/s, frames/s, camera fps, UI ms/s and the capture-thread pipeline ms/s.
4. If ClassIn can be closed: one realistic run without `OVC_TEST` (camera + virtual camera
   + MarkCam Microphone), visible and minimized.
5. Report the same table, old vs new.

---
## Round 1 (done)

Context: MarkCam is the user's own ManyCam replacement (Rust, egui; virtual camera
"MarkCam" via a DirectShow filter; "MarkCam Microphone" via VB-CABLE). It was developed and
measured only on a desktop (Ryzen 5 2600X, GTX 1070): with a 720p30 camera it uses ~2% of
the whole CPU (≈ 25% of one core) and ~2% GPU. It is now installed on this laptop
(7th-gen Intel) from MarkCam-Setup-1.0.0.exe into `C:\Program Files\MarkCam`, and Task
Manager shows CPU ~27% and GPU ~20% alternating with 0. Find out why. Reply to the user in
Serbian (Latin); they don't want to be asked questions — just measure and report.

Do NOT feed the virtual camera into a running class: run measurements with ClassIn closed,
or with the test switches below (`OVC_TEST=1` = virtual camera off, no audio engine).

## 1. Machine
```powershell
(Get-CimInstance Win32_Processor).Name; [Environment]::ProcessorCount
Get-CimInstance Win32_VideoController | Select Name, DriverVersion
powercfg /getactivescheme
(Get-CimInstance Win32_Battery).BatteryStatus   # 1 = on battery, 2 = charging
```

## 2. The running MarkCam as the user uses it (camera on, virtual camera on)
Per-second CPU (percent of the whole machine) and GPU for 15 s:
```powershell
$p = Get-Process markcam | Select -First 1
for ($i=0;$i -lt 15;$i++) { $a=$p.TotalProcessorTime
  $g=(Get-Counter "\GPU Engine(pid_$($p.Id)*)\Utilization Percentage" -SampleInterval 1 -MaxSamples 1).CounterSamples | Measure-Object CookedValue -Sum
  $p.Refresh(); "{0,5:N1}% CPU  {1,5:N1}% GPU" -f (($p.TotalProcessorTime-$a).TotalSeconds/[Environment]::ProcessorCount*100), $g.Sum }
```
Also note the camera format chosen (top bar resolution / fps) and the status bar text at
the bottom of MarkCam (camera fps, preview fps, GPU adapter name), and whether anything is
on the picture (effects, layers), Mirror, and which tab is open.

## 3. Isolate the parts (each 15 s, same loop as above on the new process)
Close the real MarkCam first.
```powershell
$exe = 'C:\Program Files\MarkCam\MarkCam.exe'
# a) test pattern only (no camera, no virtual camera, no audio)
$env:OVC_TEST='1'; $env:OVC_SETTINGS="$env:TEMP\mc-perf.json"; '{}' | Out-File -Encoding ascii $env:OVC_SETTINGS
Start-Process $exe
# b) real camera, still no virtual camera / audio:   $env:OVC_CAMERA='0'  (then Start-Process again)
# c) MarkCam minimized (preview is skipped when minimized) vs. in the background behind another window
```
Compare a/b/c with the normal run in step 2. The difference between 2 and b is the
virtual camera + the audio engine (MarkCam Microphone mixer).

## 4. Report
A small table (scenario → CPU %, GPU %), the CPU/GPU model, power plan / battery state,
and the camera's resolution/fps. The developer session on the desktop will then optimize
(ideas already noted: render the preview less often when MarkCam isn't the focused window;
move processing off the UI thread so the egui repaint isn't tied to the camera rate).
