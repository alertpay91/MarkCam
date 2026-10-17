# MarkCam

**A free virtual camera with live, fun effects for online teaching** (ClassIn, Zoom, Teams…),
made by a teacher, for teachers. Windows 10 / 11 and Linux (Linux Mint 22, Ubuntu 24.04).

- **Effects that follow your face:** 40+ animated reaction faces, animal masks, characters
  (baby, grandpa, astronaut…), realistic hair and beards, crowns, rain clouds over your head,
  stars flying around your head, big eyes, big ears, a third eye.
- **Effect Maker:** build your own face effect from emoji, pictures, GIFs or a photo (its
  background is removed automatically), place it on your face with the mouse, animate it,
  save it and share it as one `.markcam` file.
- **Name banners:** your name at the bottom of the picture in 3D styles, sliding in and out
  (any text, Chinese too). Key **N**.
- **Classroom phrases:** 84 speech bubbles in English with a Chinese translation.
- **Weather:** rain, snow, thunderstorm, wind, sunshine, fog — over the whole picture.
- **Virtual background** with AI (no green screen needed) or a color key.
- **Play videos from links** (YouTube…) into the class with sound, through the
  *MarkCam Microphone*. Share part of your screen. Freeze the picture.
- **Studio voice:** AI noise suppression for your microphone, like Krisp — *Light* (RNNoise)
  takes out fans, air conditioning and hum; *Best* (DeepFilterNet) also keyboard clicks and other
  sudden noises. Plus rumble filter, even loudness and no clipping. Only your voice is cleaned:
  videos and music you play stay untouched, noise is lowered but never muted, and quiet sound is
  never pumped up. A 5-second test lets you hear before / after.
- Light on the CPU and battery.

## Download

Get it from **[Releases](https://github.com/alertpay91/MarkCam/releases)**:

- **Windows:** `MarkCam-Setup-…-public.exe`. Windows may say "unknown publisher" — click
  *More info → Run anyway*.
- **Linux Mint / Ubuntu:** `markcam_…_amd64.deb` — double-click it, or
  `sudo apt install ./markcam_*.deb`. apt brings what it needs (v4l2loopback for the
  "MarkCam" camera, ffmpeg, slop). With Secure Boot on, enroll the key when asked after the
  restart. The *MarkCam Microphone* is one click in the Audio tab (PulseAudio / PipeWire,
  no driver). Screen sharing needs the normal X11 session (Mint's default).

## Build

Rust (stable, `x86_64-pc-windows-gnu`), NASM. `cargo build --release -p ovc-app -p ovc-dshow`.
The installer: `installer\build-installer.ps1 -Version x.y.z -Public` (Inno Setup 6).
Linux: `installer\build-deb.ps1 -Version x.y.z` builds the `.deb` through WSL (Ubuntu 24.04),
or run `installer/linux/build-deb.sh` on Linux itself (needs `libpulse-dev`, `libgtk-3-dev`,
`libxkbcommon-dev`, `libclang-dev`, `nasm`).
Effect art: `tools\effects-art` (see `docs/EFFECTS.md`).

## License

MarkCam is free software under the **GNU GPL v3 or later** (see `LICENSE`).
Third-party parts keep their own licenses: see `THIRD-PARTY-NOTICES.txt`.

MarkCam was written by Marko Stevanović together with Claude, an AI assistant made by Anthropic.
It is an independent project, not affiliated with or endorsed by Anthropic, ManyCam, VB-Audio,
Microsoft, Google or YouTube.

## Support

If MarkCam makes your lessons more fun, you can buy me a coffee via PayPal:
alertpay91@gmail.com (also from *About MarkCam* in the app).