; Inno Setup script for Veyra.
; Build: "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\veyra.iss
; (after `cargo build --release`). Keep the AppId: updates rely on it.

#define AppName "Veyra"
#define AppVersion "0.6.1"
#define AppPublisher "MooseDevPJS"
#define AppURL "https://github.com/MooseDevPJS/veyra"
#define AppExe "veyra.exe"

[Setup]
AppId={{E9828310-D2CD-4020-8F91-4B8584541B02}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
; Per-user install by default: no administrator rights needed.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile=..\LICENSE
OutputDir=Output
OutputBaseFilename=Veyra-{#AppVersion}-setup
SetupIconFile=..\assets\logo\veyra.ico
WizardImageFile=wizard-large.bmp,wizard-large-2x.bmp
WizardSmallImageFile=wizard-small.bmp,wizard-small-2x.bmp
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "italian"; MessagesFile: "compiler:Languages\Italian.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\assets\ubo-resources.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\ui\*"; Excludes: "logo-render.html"; DestDir: "{app}\ui"; Flags: ignoreversion recursesubdirs
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; Update from the app (silent installer): Veyra opens again by itself.
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: WizardSilent
