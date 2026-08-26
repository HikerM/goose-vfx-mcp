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
#ifndef MyAppPublisher
  #define MyAppPublisher "Lumina contributors"
#endif
#define MyAppExeName "Lumina.exe"

[Setup]
AppId={{B65352CC-4606-40E0-BD54-06AFE4B06392}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppCopyright=Copyright (C) 2026 Lumina contributors
DefaultDirName={localappdata}\Programs\Lumina
DisableDirPage=no
UsePreviousAppDir=yes
DirExistsWarning=no
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
Root: HKCU; Subkey: "Software\Lumina"; ValueType: string; ValueName: "InstallLocation"; ValueData: "{app}"; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Classes\lumina"; ValueType: string; ValueName: ""; ValueData: "URL:Lumina Protocol"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\lumina"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\lumina\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\{#MyAppExeName},0"
Root: HKCU; Subkey: "Software\Classes\lumina\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#StringChange(MyAppName, '&', '&&')}}"; Flags: nowait postinstall skipifsilent

[Code]
function IsDirectoryEntry(const FindRec: TFindRec): Boolean;
begin
  Result := (FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0;
end;

function IsDotEntry(const Name: String): Boolean;
begin
  Result := (Name = '.') or (Name = '..');
end;

function IsLegacyProgramEntry(const Name: String): Boolean;
var
  Normalized: String;
begin
  Normalized := Lowercase(Name);
  Result :=
    (Pos('app-', Normalized) = 1) or
    (Normalized = 'packages') or
    (Normalized = 'update.exe') or
    (Normalized = 'squirrelsetup.log') or
    (Normalized = '.dead');
end;

function IsGovernedDataEntry(const Name: String): Boolean;
var
  Normalized: String;
begin
  Normalized := Lowercase(Name);
  Result :=
    (Normalized = 'config') or
    (Normalized = 'data') or
    (Normalized = 'state') or
    (Normalized = '.agents');
end;

procedure CopyDirectoryPreserving(
  const SourceDir: String;
  const DestinationDir: String;
  const PreserveExisting: Boolean
);
var
  FindRec: TFindRec;
  SourcePath: String;
  DestinationPath: String;
begin
  if not DirExists(SourceDir) then
    Exit;
  ForceDirectories(DestinationDir);
  if FindFirst(SourceDir + '\*', FindRec) then
  begin
    try
      repeat
        if not IsDotEntry(FindRec.Name) then
        begin
          SourcePath := SourceDir + '\' + FindRec.Name;
          DestinationPath := DestinationDir + '\' + FindRec.Name;
          if IsDirectoryEntry(FindRec) then
            CopyDirectoryPreserving(SourcePath, DestinationPath, PreserveExisting)
          else if not FileCopy(SourcePath, DestinationPath, PreserveExisting) then
            Log('保留文件失败：' + SourcePath);
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
end;

procedure BackupLocalLegacyUserContent(const SourceDir: String; const BackupDir: String);
var
  FindRec: TFindRec;
  SourcePath: String;
  DestinationPath: String;
begin
  if not DirExists(SourceDir) then
    Exit;
  ForceDirectories(BackupDir);
  if FindFirst(SourceDir + '\*', FindRec) then
  begin
    try
      repeat
        if (not IsDotEntry(FindRec.Name)) and (not IsLegacyProgramEntry(FindRec.Name)) then
        begin
          SourcePath := SourceDir + '\' + FindRec.Name;
          DestinationPath := BackupDir + '\' + FindRec.Name;
          if IsDirectoryEntry(FindRec) then
            CopyDirectoryPreserving(SourcePath, DestinationPath, False)
          else if not FileCopy(SourcePath, DestinationPath, False) then
            Log('备份旧版用户文件失败：' + SourcePath);
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
end;

function GovernedDataRoot(): String;
begin
  Result := GetEnv('LUMINA_PATH_ROOT');
  if Result = '' then
    Result := 'D:\Lumina';
end;

procedure RestoreLocalLegacyUserContent(const BackupDir: String);
var
  FindRec: TFindRec;
  SourcePath: String;
  DestinationPath: String;
  DataRoot: String;
begin
  if not DirExists(BackupDir) then
    Exit;
  DataRoot := GovernedDataRoot();
  ForceDirectories(DataRoot);
  ForceDirectories(DataRoot + '\desktop');
  if FindFirst(BackupDir + '\*', FindRec) then
  begin
    try
      repeat
        if not IsDotEntry(FindRec.Name) then
        begin
          SourcePath := BackupDir + '\' + FindRec.Name;
          if IsGovernedDataEntry(FindRec.Name) then
            DestinationPath := DataRoot + '\' + FindRec.Name
          else
            DestinationPath := DataRoot + '\desktop\' + FindRec.Name;
          if IsDirectoryEntry(FindRec) then
            CopyDirectoryPreserving(SourcePath, DestinationPath, True)
          else if not FileCopy(SourcePath, DestinationPath, True) then
            Log('目标已有文件，保留当前版本：' + DestinationPath);
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
end;

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

procedure MigrateLegacyInstallation();
var
  LegacyRoot: String;
  LegacyUpdater: String;
  LegacyRoamingData: String;
  BackupRoot: String;
  ResultCode: Integer;
begin
  LegacyRoot := ExpandConstant('{localappdata}\Lumina');
  LegacyUpdater := LegacyRoot + '\Update.exe';
  LegacyRoamingData := ExpandConstant('{userappdata}\Lumina');
  BackupRoot := ExpandConstant('{localappdata}\LuminaUpgradeBackup\{#MyAppVersion}');

  BackupLocalLegacyUserContent(LegacyRoot, BackupRoot + '\local-user-data');
  CopyDirectoryPreserving(LegacyRoamingData, BackupRoot + '\roaming-user-data', False);

  if FileExists(LegacyUpdater) then
  begin
    Exec(LegacyUpdater, '--uninstall -s', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Log(Format('旧版 Squirrel 卸载返回码 %d；用户数据备份保留在 %s', [ResultCode, BackupRoot]));
  end;

  RestoreLocalLegacyUserContent(BackupRoot + '\local-user-data');
  CopyDirectoryPreserving(
    BackupRoot + '\roaming-user-data',
    GovernedDataRoot() + '\desktop',
    True
  );
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  StopRunningLumina();
  MigrateLegacyInstallation();
  Result := '';
end;
