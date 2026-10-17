MarkCam for Linux
=================
Virtual camera & live video effects for online lessons (Zoom, Google Meet, Teams,
ClassIn in the browser, OBS...). Linux Mint 22 or newer, Ubuntu 24.04 or newer and
other systems based on them, 64-bit.

INSTALL
  Double-click markcam_<version>_amd64.deb (or: sudo apt install ./markcam_*.deb).
  apt also installs what MarkCam needs: v4l2loopback (the virtual camera driver),
  ffmpeg, slop (the area picker) and pulseaudio-utils.
  With Secure Boot on, the driver install asks for a password once and, after the
  restart, shows a blue "Enroll MOK" screen: pick Enroll MOK, Continue, enter that
  password. Without Secure Boot nothing more to do.

THE CAMERA
  Apps see a camera named "MarkCam" (it is ready right after the install, and at every
  start of the computer). Pick it in Zoom / Meet / the browser. Top right in MarkCam
  shows "LIVE" while an app is using it; MarkCam sends no frames while nobody is
  watching (no CPU spent). If "Install virtual camera" shows instead, click it (asks
  for your password) - it loads the driver now.

THE MICROPHONE
  Audio tab > "Create MarkCam Microphone": a microphone named "MarkCam Microphone"
  (PulseAudio / PipeWire, no driver, no password) that carries your voice plus the
  sound of the video / YouTube link you play. MarkCam recreates it by itself after a
  restart. Pick it as the microphone in the lesson app while MarkCam runs.

SCREEN AS THE PICTURE
  Source > Full screen / "Select area..." works in the normal (X11) session - Linux
  Mint's Cinnamon default. On Wayland sessions screen capture is not available.

EVERYTHING ELSE
  Same as on Windows: effects that follow your face, virtual backgrounds (Edge
  quality: Light = fast on any laptop, Best = the strongest model), name banners,
  Effect Maker, classroom bubbles (English + Chinese), videos and links as the
  picture, Freeze, Camera boost. Your data lives in ~/.config/MarkCam.

UNINSTALL: sudo apt remove markcam  (your ~/.config/MarkCam stays).

MarkCam is free software: GNU GPL v3 or later (LICENSE.txt). The source code is in the
"source" folder and at https://github.com/alertpay91/MarkCam. Made by Marko Stevanovic
together with Claude (an AI assistant by Anthropic). Third-party parts and their
licenses: THIRD-PARTY-NOTICES.txt and the licenses folder.
Only play or download videos you have the right to use.
