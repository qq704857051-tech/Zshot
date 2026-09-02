; Zshot installer — build with:
;   "C:\Users\admin\AppData\Local\Programs\Inno Setup 6\ISCC.exe" installer\zshot.iss
; Payload mirrors the portable layout: Zshot.exe + DirectML.dll + models\v6,
; which is exactly what ocr::models_dir expects next to the exe.

// release.yml injects /DAppVersion=<tag>; the fallback matches Cargo.toml.
#ifndef AppVersion
#define AppVersion "0.2.0"
#endif

[Setup]
AppId={{8F3A5C2E-9B7D-4E1F-A6C3-2D5E8F0B1A47}
AppName=Zshot
AppVersion={#AppVersion}
AppPublisher=Zyl0812
AppPublisherURL=https://github.com/Zyl0812/Zshot
AppSupportURL=https://github.com/Zyl0812/Zshot/issues
AppUpdatesURL=https://github.com/Zyl0812/Zshot/releases/latest
; Per-user install: no UAC prompt, matches how a tray screenshot tool is used.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\Zshot
DefaultGroupName=Zshot
OutputDir=..\dist
OutputBaseFilename=Zshot-v{#AppVersion}-x64-setup
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\Zshot.exe
SetupIconFile=..\assets\logo\logo.ico
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "chinesesimplified"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "autostart"; Description: "开机自动启动"; GroupDescription: "其他选项:"; Flags: unchecked

[Files]
Source: "..\target\release\Zshot.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\DirectML.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\models\v6\*"; DestDir: "{app}\models\v6"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Zshot"; Filename: "{app}\Zshot.exe"
Name: "{group}\卸载 Zshot"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Zshot"; Filename: "{app}\Zshot.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Zshot"; ValueData: """{app}\Zshot.exe"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\Zshot.exe"; Description: "{cm:LaunchProgram,Zshot}"; Flags: nowait postinstall skipifsilent
