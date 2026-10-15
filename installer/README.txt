MarkCam
=======
Virtual camera & live video effects for online lessons (ClassIn, Zoom, OBS...).
Windows 10 and Windows 11 (64-bit).

INSTALLED FOR YOU
  - The "MarkCam" camera: in ClassIn / Zoom pick the camera named "MarkCam"
    (restart ClassIn once after installing). Top right shows "LIVE 1280x720"
    while ClassIn is using it.
  - The "MarkCam Microphone": in ClassIn pick it as the microphone while MarkCam
    runs. It carries your voice + the sound of the video / YouTube you play
    (volumes in the Audio tab). Switch back to your own microphone when MarkCam
    is closed.
  - The video tools (yt-dlp, ffmpeg) and face-tracking models - nothing more to
    download. yt-dlp updates itself so YouTube keeps working.
  - MarkCam's own effects: animated reaction faces, animal masks, characters, hair
    and beards, fun things on your head (all follow your face), hands and stickers,
    animals, food, things and places, classroom speech bubbles (English + Chinese)
    and frames - plus "My effects" and your own backgrounds.

Everything you set (camera, effects on the picture, categories...) is remembered.
Your data lives in %APPDATA%\MarkCam and stays when MarkCam is uninstalled.

TOP BAR
  - Source menu: cameras, Full screen, "Select area..." (e.g. a student's video
    in ClassIn - face effects then apply to that face), a video file, or
    "Play from link" (YouTube, Twitch, Facebook... live too; pick 360p-1080p,
    or "Download" to save it in Videos\MarkCam).
  - Mirror: flips only your preview; others always see you the right way round.
  - Freeze (key F): holds the picture still while effects, GIFs and masks keep
    moving - for a quick stretch during a lesson. Click again to go live.

PLAYING A VIDEO / LINK
  - The bar under the picture: Play / Pause (Space), back / forward 10 s, and a
    slider to jump anywhere.

EFFECTS (right panel)
  - Search all effects, browse categories, or MarkCam's own Animated (Fire,
    Matrix, Snow, Rain, Balloons...), Distortions (Bulge, Swirl...) and Color filters.
  - "Make your own effect" (Effect Maker): build a face effect from emoji, pictures,
    GIFs or a photo, place it with the mouse, animate it, save and share (.markcam).
  - "Name banner" (key N, or the Name button on top): your name at the bottom of the
    picture, in 3D styles, any text.
  - Your own face sticker: put a PNG in My effects with a "<name>.face.json" next to it:
    { "center": [x, y], "size": [w, h] } in face units (eyes at y = 0, x = +-0.6;
    mouth y = 1.2; top of the head y = -1.3).
  - "+" or drag & drop: add your own PNG / JPG / GIF / WebP / APNG / MP4.
  - Right-click an effect: move it to a category, or delete it (your own files
    go to the Recycle Bin; library effects can be restored with "Restore deleted
    effects" at the bottom of the category list).
  - Right-click a category (or open it and use Rename / Delete at the top):
    rename or delete it. "+ New category" makes your own.
  - On the picture: click an effect to select it, drag to move, scroll to resize,
    Delete to remove. Face effects (hats, glasses, masks) follow your face.

BACKGROUND (right panel)
  - None, Blur, MarkCam's own backdrops (pictures and animated ones), or your own
    ("+ Add your own..."; right-click one of yours to delete it).
  - An AI video matting model (RVM; MODNet as fallback) finds you in every frame,
    on the graphics chip when possible; head, arms and hands are followed as you
    move, and each frame is shown with its own outline (no trailing edges).
  - Edge quality: Auto (Best on a strong graphics chip, else Light), Best (cleaner
    fingers and hands in motion), Light (laptops, longer battery life).
  - Good light in front of you helps the most: in dim light the webcam drops to
    15 fps and the picture gets noisy.

SETUP (right panel)
  - Preview fps while MarkCam is behind another window, "Rest the webcam" when
    nothing shows the picture, "Start MarkCam with Windows", and where
    downloaded videos are saved.

UNINSTALL: Windows Settings > Apps > MarkCam. The MarkCam Microphone (VB-CABLE)
stays installed; to remove it too, run VBCABLE_Setup_x64.exe from the VB-CABLE
download (vb-audio.com) as administrator and click "Remove Driver".

MarkCam is free software: GNU GPL v3 or later (LICENSE.txt). The source code is
in the "source" folder (and linked from About MarkCam). Made by Marko Stevanovic
together with Claude (an AI assistant by Anthropic).
Third-party parts and their licenses: THIRD-PARTY-NOTICES.txt and the licenses
folder. VB-CABLE is donationware by VB-Audio (www.vb-cable.com).
Only play or download videos you have the right to use.
