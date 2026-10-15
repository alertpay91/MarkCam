# MarkCam

**A free virtual camera with live, fun effects for online teaching** (ClassIn, Zoom, Teams…),
made by a teacher, for teachers. Windows 10 / 11.

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
- Light on the CPU and battery.

## Download

Get the installer from **[Releases](https://github.com/alertpay91/MarkCam/releases)**
(`MarkCam-Setup-…-public.exe`). Windows may say "unknown publisher" — click *More info → Run anyway*.

## Build

Rust (stable, `x86_64-pc-windows-gnu`), NASM. `cargo build --release -p ovc-app -p ovc-dshow`.
The installer: `installer\build-installer.ps1 -Version x.y.z -Public` (Inno Setup 6).
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