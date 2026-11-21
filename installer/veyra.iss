; Inno Setup script for Veyra.
; Build: "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" installer\veyra.iss
; (after `cargo build --release`). Keep the AppId: updates rely on it.

#define AppName "Veyra"
#define AppVersion "0.8.2"
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
ChangesAssociations=yes

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

[Registry]
; Veyra as a browser Windows can make the default (Settings > Default apps).
; Same keys the app writes from Settings (src/default_browser.rs).
Root: HKA; Subkey: "Software\Classes\VeyraURL"; ValueType: string; ValueName: ""; ValueData: "Veyra URL"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\VeyraURL"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKA; Subkey: "Software\Classes\VeyraURL\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Classes\VeyraURL\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\VeyraURL\Application"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"
Root: HKA; Subkey: "Software\Classes\VeyraHTML"; ValueType: string; ValueName: ""; ValueData: "HTML"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\VeyraHTML\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Classes\VeyraHTML\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""
Root: HKA; Subkey: "Software\Classes\VeyraHTML\Application"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra"; ValueType: string; ValueName: ""; ValueData: "{#AppName}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"""
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "{#AppName}"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "A light browser with no AI"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities"; ValueType: string; ValueName: "ApplicationIcon"; ValueData: """{app}\{#AppExe}"",0"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities\StartMenu"; ValueType: string; ValueName: "StartMenuInternet"; ValueData: "Veyra"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities\URLAssociations"; ValueType: string; ValueName: "http"; ValueData: "VeyraURL"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities\URLAssociations"; ValueType: string; ValueName: "https"; ValueData: "VeyraURL"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities\FileAssociations"; ValueType: string; ValueName: ".htm"; ValueData: "VeyraHTML"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities\FileAssociations"; ValueType: string; ValueName: ".html"; ValueData: "VeyraHTML"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities\FileAssociations"; ValueType: string; ValueName: ".shtml"; ValueData: "VeyraHTML"
Root: HKA; Subkey: "Software\Clients\StartMenuInternet\Veyra\Capabilities\FileAssociations"; ValueType: string; ValueName: ".xhtml"; ValueData: "VeyraHTML"
Root: HKA; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Veyra"; ValueData: "Software\Clients\StartMenuInternet\Veyra\Capabilities"; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.htm\OpenWithProgids"; ValueType: string; ValueName: "VeyraHTML"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.html\OpenWithProgids"; ValueType: string; ValueName: "VeyraHTML"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.shtml\OpenWithProgids"; ValueType: string; ValueName: "VeyraHTML"; ValueData: ""; Flags: uninsdeletevalue
Root: HKA; Subkey: "Software\Classes\.xhtml\OpenWithProgids"; ValueType: string; ValueName: "VeyraHTML"; ValueData: ""; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent
; Update from the app (silent installer): Veyra opens again by itself.
Filename: "{app}\{#AppExe}"; Flags: nowait; Check: WizardSilent
