; MarkCam — Windows 10 / 11 installer (Inno Setup 6).
; Build with installer\build-installer.ps1, which gathers everything into installer\stage
; and runs ISCC. Installs the app, registers the "MarkCam" virtual camera (DirectShow) and
; installs + names the "MarkCam Microphone" (VB-CABLE), with the video tools, face models
; and the effects library.

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif
#ifndef Suffix
  #define Suffix ""
#endif
#define Stage "stage"

[Setup]
AppId={{6E1B7C2A-4F0D-4E8B-9A57-3C1D2E8F4B90}
AppName=MarkCam
AppVersion={#AppVersion}
AppVerName=MarkCam {#AppVersion}
AppPublisher=Marko Stevanovic
DefaultDirName={autopf}\MarkCam
DefaultGroupName=MarkCam
DisableProgramGroupPage=yes
DisableDirPage=auto
OutputDir=..\dist
OutputBaseFilename=MarkCam-Setup-{#AppVersion}{#Suffix}
SetupIconFile=..\crates\ovc-app\icon.ico
UninstallDisplayIcon={app}\MarkCam.exe
UninstallDisplayName=MarkCam
Compression=lzma2/max
SolidCompression=yes
LZMANumBlockThreads=4
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
MinVersion=10.0
WizardStyle=modern
; Close MarkCam before replacing files. Only .exe files count: every browser and ClassIn
; that has listed cameras holds markcam_dshow.dll, and asking to close those (or, silently,
; aborting) is worse than replacing the DLL at the next restart (restartreplace below).
CloseApplications=yes
CloseApplicationsFilter=*.exe
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "audio"; Description: "Install the MarkCam &Microphone (VB-CABLE by VB-Audio, donationware - www.vb-cable.com) for video sound in ClassIn / Zoom"; GroupDescription: "Drivers:"

[Files]
Source: "{#Stage}\MarkCam.exe"; DestDir: "{app}"; Flags: ignoreversion
; The virtual camera: registered with Windows (regsvr32) on install, unregistered on uninstall.
; If ClassIn / a browser still holds the old one, it is replaced at the next restart.
Source: "{#Stage}\markcam_dshow.dll"; DestDir: "{app}"; Flags: ignoreversion regserver restartreplace uninsrestartdelete
Source: "{#Stage}\README.txt"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "{#Stage}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\source\*"; DestDir: "{app}\source"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#Stage}\models\*"; DestDir: "{app}\models"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Stage}\tools\*"; DestDir: "{app}\tools"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Stage}\drivers\*"; DestDir: "{app}\drivers"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Stage}\effects\*"; DestDir: "{app}\effects"; Flags: ignoreversion recursesubdirs createallsubdirs
; Virtual background: ONNX Runtime + DirectML (loaded by MarkCam at run time), licenses, and
; the bundled background pictures.
Source: "{#Stage}\onnxruntime.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\onnxruntime_providers_shared.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\DirectML.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Stage}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion
Source: "{#Stage}\backgrounds\*"; DestDir: "{app}\backgrounds"; Flags: ignoreversion skipifsourcedoesntexist

[Icons]
Name: "{autoprograms}\MarkCam"; Filename: "{app}\MarkCam.exe"
Name: "{autodesktop}\MarkCam"; Filename: "{app}\MarkCam.exe"; Tasks: desktopicon

[Run]
; VB-CABLE (skipped if already installed), renamed to "MarkCam Microphone"; the user's
; default speakers / microphone are put back afterwards. Windows may ask to confirm the
; driver (publisher: Vincent Burel / VB-Audio).
Filename: "{app}\MarkCam.exe"; Parameters: "--setup-audio ""{app}\drivers\vbcable"""; StatusMsg: "Installing the MarkCam Microphone (confirm the Windows prompt if one appears)..."; Flags: runhidden waituntilterminated; Tasks: audio
Filename: "{app}\MarkCam.exe"; Description: "Start MarkCam now"; Flags: nowait postinstall skipifsilent runasoriginaluser

[InstallDelete]
; Earlier versions shipped ManyCam's effects and backgrounds: remove them on update
; (ManyCam's own folders are used instead when the user turns that on in Setup).
Type: filesandordirs; Name: "{app}\effects\Effect Packs"
Type: filesandordirs; Name: "{app}\effects\Video"
Type: filesandordirs; Name: "{app}\backgrounds"
Type: filesandordirs; Name: "{app}\effects\MarkCam"

[UninstallDelete]
; Files the app may have created next to itself (user data in %APPDATA%\MarkCam is kept).
Type: filesandordirs; Name: "{app}\tools"
