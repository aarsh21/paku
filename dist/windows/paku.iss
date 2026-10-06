; Paku for Windows — per-user installer (Inno Setup 6).
;
; Built by scripts/package-windows.ps1, which passes the version, the package
; architecture, and the staged portable directory:
;   ISCC.exe /DAppVersion=0.2.97 /DArch=x86_64 /DPackageDir=<stage> /DOutputDir=<out> paku.iss
;
; Installs into %LOCALAPPDATA%\Programs\Paku without elevation, like VS
; Code's user setup: the directory stays writable by its user, so the in-app
; updater (crates/update/src/windows.rs) can replace paku.exe in place. The
; staged directory already carries paku-update.json, which marks the install
; as update-managed. Re-running a newer installer upgrades in place; user data
; lives in %LOCALAPPDATA%\Paku and is never touched here.

#ifndef AppVersion
  #error AppVersion must be defined (/DAppVersion=x.y.z)
#endif
#ifndef Arch
  #error Arch must be defined (/DArch=x86_64 or /DArch=aarch64)
#endif
#ifndef PackageDir
  #error PackageDir must be defined (/DPackageDir=<staged package directory>)
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

#if Arch == "aarch64"
  #define ArchAllowed "arm64"
#else
  #define ArchAllowed "x64compatible"
#endif

[Setup]
; Paku has a distinct installation identity; never reuse the upstream AppId.
; Keep this UUID stable across Paku upgrades and in crates/update/src/windows.rs.
AppId={{d98c3134-ef43-4bdb-94b8-1d892301a381}
AppName=Paku
AppVersion={#AppVersion}
AppVerName=Paku {#AppVersion}
AppPublisher=Paku
AppPublisherURL=https://github.com/aarsh21/paku
AppSupportURL=https://github.com/aarsh21/paku/issues
AppUpdatesURL=https://github.com/aarsh21/paku/releases
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\Paku
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableReadyPage=yes
ArchitecturesAllowed={#ArchAllowed}
ArchitecturesInstallIn64BitMode={#ArchAllowed}
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=paku-{#AppVersion}-windows-{#Arch}-setup
SetupIconFile=paku.ico
UninstallDisplayIcon={app}\paku.exe
UninstallDisplayName=Paku
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; A running Paku is closed through the Restart Manager before its files are
; replaced; the updated app starts again from the finish page.
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#PackageDir}\paku.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\paku-update.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PackageDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Paku"; Filename: "{app}\paku.exe"
Name: "{autodesktop}\Paku"; Filename: "{app}\paku.exe"; Tasks: desktopicon

[Registry]
; paku:// conversation links — the scheme macOS registers in Info.plist and
; Linux in paku.desktop.
Root: HKCU; Subkey: "Software\Classes\paku"; ValueType: string; ValueName: ""; ValueData: "URL:Paku"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\paku"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\paku\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\paku.exe"",0"
Root: HKCU; Subkey: "Software\Classes\paku\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\paku.exe"" ""%1"""

[Run]
Filename: "{app}\paku.exe"; Description: "{cm:LaunchProgram,Paku}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Leftovers of in-app updates (crates/update/src/windows.rs).
Type: files; Name: "{app}\paku.exe.old"
Type: files; Name: "{app}\.paku-update-incoming.exe"
Type: filesandordirs; Name: "{app}\.paku-update-*"
