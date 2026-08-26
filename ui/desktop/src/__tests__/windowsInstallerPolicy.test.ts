import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const installer = readFileSync(resolve(__dirname, '../../installer/lumina.iss'), 'utf8');
const installerBuilder = readFileSync(
  resolve(__dirname, '../../scripts/build-windows-installer.js'),
  'utf8'
);
const windowsWorkflow = readFileSync(
  resolve(__dirname, '../../../../.github/workflows/bundle-desktop-windows.yml'),
  'utf8'
);

describe('Windows installer upgrade policy', () => {
  it('keeps a stable application identity and lets users choose the install directory', () => {
    expect(installer).toContain('AppId={{B65352CC-4606-40E0-BD54-06AFE4B06392}');
    expect(installer).toContain('DisableDirPage=no');
    expect(installer).toContain('UsePreviousAppDir=yes');
    expect(installer).toContain('ValueName: "InstallLocation"; ValueData: "{app}"');
  });

  it('backs up user content before uninstalling the legacy package', () => {
    const backup = installer.indexOf('BackupLocalLegacyUserContent(LegacyRoot');
    const uninstall = installer.indexOf("Exec(LegacyUpdater, '--uninstall -s'");
    const restore = installer.indexOf('RestoreLocalLegacyUserContent(BackupRoot');

    expect(backup).toBeGreaterThan(0);
    expect(uninstall).toBeGreaterThan(backup);
    expect(restore).toBeGreaterThan(uninstall);
    expect(installer).not.toContain('DelTree(LegacyRoot');
  });

  it('restores without overwriting data already present in the governed root', () => {
    expect(installer).toContain("Result := GetEnv('LUMINA_PATH_ROOT')");
    expect(installer).toContain("Result := 'D:\\Lumina'");
    expect(installer).toContain('CopyDirectoryPreserving(SourcePath, DestinationPath, True)');
    expect(installer).toContain(
      "BackupRoot := ExpandConstant('{localappdata}\\LuminaUpgradeBackup"
    );
  });

  it('publishes the installer from both signed and unsigned Windows release paths', () => {
    expect(installerBuilder).toContain('LUMINA_WINDOWS_APP_DIRECTORY');
    expect(installerBuilder).toContain("process.env.LUMINA_DESKTOP_CUDA === '1'");
    expect(windowsWorkflow.match(/Build customizable Windows installer/g)).toHaveLength(2);
    expect(
      windowsWorkflow.match(/Lumina-\*-Windows-x64-Setup\.exe/g)?.length
    ).toBeGreaterThanOrEqual(2);
    expect(windowsWorkflow).toContain('Sign Windows installer with Azure Trusted Signing');
  });
});
