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
CloseApplicationsFilter=*.exe,*.dll
RestartApplications=no
ChangesAssociations=yes
SetupLogging=yes
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription={#MyAppName} 安装程序
VersionInfoProductName={#MyAppName}
VersionInfoVersion={#MyFileVersion}
VersionInfoProductVersion={#MyFileVersion}
VersionInfoTextVersion={#MyAppVersion}

[Languages]
Name: "chinesesimp"; MessagesFile: "compiler:Default.isl,languages\ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

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
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[Code]
procedure StopRunningLumina();
var
  Attempt: Integer;
  ResultCode: Integer;
begin
  for Attempt := 1 to 3 do
  begin
    Exec(
      ExpandConstant('{cmd}'),
      '/D /C taskkill.exe /F /T /IM "Lumina.exe" >nul 2>&1',
      '',
      SW_HIDE,
      ewWaitUntilTerminated,
      ResultCode
    );
    Log(Format('关闭 Lumina 进程：第 %d 次，taskkill 返回码 %d', [Attempt, ResultCode]));
    Sleep(500);
  end;
end;

procedure RemoveLegacyInstallation();
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
    Log('无法完全删除旧版 Squirrel 安装目录');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopRunningLumina();
  RemoveLegacyInstallation();
  Result := '';
end;
