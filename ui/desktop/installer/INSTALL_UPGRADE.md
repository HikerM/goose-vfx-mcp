# Windows installation and upgrade contract

Lumina uses a stable Inno Setup `AppId`, so installing a newer version detects the existing installation and upgrades it in place. The directory page is always available: interactive users may keep the previous location or choose another writable location. Automated deployments may pass Inno Setup's `/DIR="D:\\Apps\\Lumina"` option.

The program directory and the user-data directory are deliberately separate:

- program files: the location chosen in the installer;
- governed Lumina state: `LUMINA_PATH_ROOT`, or `D:\\Lumina` when the variable is not set;
- desktop user data restored from older packages: `<governed-root>\\desktop`;
- upgrade backup: `%LOCALAPPDATA%\\LuminaUpgradeBackup\\<version>`.

Before invoking a legacy Squirrel uninstaller, setup copies non-program content from `%LOCALAPPDATA%\\Lumina` and all `%APPDATA%\\Lumina` content into the versioned backup. It then restores data without overwriting files already present in the governed root. Setup never recursively deletes the legacy root.

Release validation must include:

1. a clean install to the default directory;
2. a clean install to a custom directory;
3. an upgrade in the same directory;
4. an upgrade while changing the program directory;
5. recovery of config, sessions, project work, credentials references, and desktop preferences;
6. rollback using the retained versioned backup;
7. silent installation with `/DIR` and setup logging enabled.

Uninstalling Lumina removes installed program files and registrations. User project files and the governed data root are not installer-owned and must remain intact.
