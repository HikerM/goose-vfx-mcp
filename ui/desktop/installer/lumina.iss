#ifndef MyAppVersion
  #error MyAppVersion must be provided by the build script
#endif
#ifndef MyFileVersion
  #error MyFileVersion must be provided by the build script
#endif
#ifndef SourceDir
  #error SourceDir must be provided by the build script
#endif
#ifndef OutputDir
  #error OutputDir must be provided by the build script
#endif

#define MyAppName "Lumina"
#define MyAppPublisher "Lumina"
#define MyAppExeName "Lumina.exe"

[Setup]
AppId={{B65352CC-4606-40E0-BD54-06AFE4B06392}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppCopyright=Copyright (C) 2026 Lumina
DefaultDirName={localappdata}\Programs\Lumina
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Lumina-{#MyAppVersion}-Windows-x64-Setup
SetupIconFile=..\src\images\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2/max
SolidCompression=yes
LZMAUseSeparateProcess=yes
WizardStyle=modern dynamic
CloseApplications=yes
RestartApplications=no
ChangesAssociations=yes
SetupLogging=yes
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription={#MyAppName} Installer
VersionInfoProductName={#MyAppName}
VersionInfoVersion={#MyFileVersion}
VersionInfoProductVersion={#MyFileVersion}
VersionInfoTextVersion={#MyAppVersion}

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Classes\goose"; ValueType: string; ValueName: ""; ValueData: "URL:Lumina Protocol"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\goose"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\goose\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#MyAppExeName},0"
Root: HKCU; Subkey: "Software\Classes\goose\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent

[Code]
function InitializeSetup(): Boolean;
var
  LegacyRoot: String;
  LegacyUpdater: String;
  ResultCode: Integer;
begin
  LegacyRoot := ExpandConstant('{localappdata}\Lumina');
  LegacyUpdater := LegacyRoot + '\Update.exe';
  if FileExists(LegacyUpdater) then
    Exec(LegacyUpdater, '--uninstall -s', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
  if DirExists(LegacyRoot) and not DelTree(LegacyRoot, True, True, True) then
    Log('Legacy Squirrel directory could not be removed completely');
  Result := True;
end;
