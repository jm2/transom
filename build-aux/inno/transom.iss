; Transom — Inno Setup installer script (scripts/build-windows.ps1 -InnoSetup)
;
; Installs the bundled dist tree as-is, keeping MSYS2's prefix shape: bin\transom.exe
; beside the GTK, libadwaita, FreeRDP and FFmpeg DLLs, share\ and lib\ beside bin\,
; so GLib and GTK find their data from their own DLL location (no environment).
; Registers the rdp: URL scheme and .rdp files (Microsoft's connection files).
;
; Preprocessor defines (iscc /D…):
;   AppVersion        Cargo package version, e.g. "0.1.0"
;   AppNumericVersion four-part numeric form, e.g. "0.1.0.0"
;   SourceDir         the bundled dist folder (dist\transom-windows)
;   OutputDir         where to write the installer
;   TargetArch        "x64" or "arm64"

#ifndef AppVersion
  #define AppVersion "0.0.1"
#endif
#ifndef AppNumericVersion
  #define AppNumericVersion "0.0.1.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\transom-windows"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif
#ifndef TargetArch
  #define TargetArch "x64"
#endif

[Setup]
AppName=Transom
AppVersion={#AppVersion}
AppVerName=Transom
; UUID v5 of the URL namespace and https://github.com/jm2/transom/io.github.jm2.Transom.
; Must never change.
AppId={{00EBE9DA-1A8A-5C74-8319-E605492192F5}
VersionInfoVersion={#AppNumericVersion}
VersionInfoProductVersion={#AppNumericVersion}
VersionInfoTextVersion={#AppVersion}
VersionInfoProductTextVersion={#AppVersion}
VersionInfoProductName=Transom
VersionInfoDescription=Transom Setup
VersionInfoCopyright=Copyright (C) 2026 Transom contributors
AppPublisher=Transom contributors
AppPublisherURL=https://github.com/jm2/transom
AppSupportURL=https://github.com/jm2/transom/issues
AppUpdatesURL=https://github.com/jm2/transom/releases
DefaultDirName={autopf}\Transom
DefaultGroupName=Transom
UninstallDisplayIcon={app}\bin\transom.exe
OutputDir={#OutputDir}
OutputBaseFilename=transom-setup
Compression=lzma2/ultra64
SolidCompression=yes
SetupIconFile=..\..\data\transom.ico
LicenseFile=..\..\COPYING
WizardStyle=modern
PrivilegesRequired=admin
PrivilegesRequiredOverridesAllowed=commandline dialog
DisableDirPage=auto
DisableProgramGroupPage=auto
CloseApplications=yes
CloseApplicationsFilter=transom.exe
ChangesAssociations=yes
SetupLogging=yes
#if TargetArch == "arm64"
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#elif TargetArch == "x64"
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
#else
  #error Unsupported TargetArch; expected x64 or arm64
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "rdpfiles"; Description: "Open .rdp files and rdp:// links with Transom"; GroupDescription: "File associations:"; Flags: unchecked

[InstallDelete]
; Upgrades replace the app-owned tree wholesale (no stale DLLs, loaders or licence
; files of packages a newer build no longer bundles). Only over an earlier Transom
; install: never in an existing folder picked for a fresh one.
Type: filesandordirs; Name: "{app}\bin"; Check: IsTransomUpgrade
Type: filesandordirs; Name: "{app}\lib"; Check: IsTransomUpgrade
Type: filesandordirs; Name: "{app}\share"; Check: IsTransomUpgrade
Type: filesandordirs; Name: "{app}\licenses"; Check: IsTransomUpgrade

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Registry]
; .rdp files (Remote Desktop Connection files) and rdp:// links, only with the (opt-in)
; rdpfiles task. HKA = HKLM for an admin install, HKCU for a per-user one. Existing
; .rdp handlers (mstsc) keep their ProgID; Transom is added to "Open with". The rdp
; key is shared with other clients, so the uninstaller removes it only while it still
; opens Transom ([Code], CurUninstallStepChanged), not with uninsdeletekey.
Root: HKA; Subkey: "Software\Classes\Transom.rdp"; ValueType: string; ValueData: "Remote Desktop Connection"; Flags: uninsdeletekey; Tasks: rdpfiles
Root: HKA; Subkey: "Software\Classes\Transom.rdp\DefaultIcon"; ValueType: string; ValueData: "{app}\bin\transom.exe,0"; Tasks: rdpfiles
Root: HKA; Subkey: "Software\Classes\Transom.rdp\shell\open\command"; ValueType: string; ValueData: """{app}\bin\transom.exe"" ""%1"""; Tasks: rdpfiles
Root: HKA; Subkey: "Software\Classes\.rdp\OpenWithProgids"; ValueType: string; ValueName: "Transom.rdp"; ValueData: ""; Flags: uninsdeletevalue; Tasks: rdpfiles
Root: HKA; Subkey: "Software\Classes\rdp"; ValueType: string; ValueData: "URL:Remote Desktop Protocol"; Tasks: rdpfiles
Root: HKA; Subkey: "Software\Classes\rdp"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""; Tasks: rdpfiles
Root: HKA; Subkey: "Software\Classes\rdp\DefaultIcon"; ValueType: string; ValueData: "{app}\bin\transom.exe,0"; Tasks: rdpfiles
Root: HKA; Subkey: "Software\Classes\rdp\shell\open\command"; ValueType: string; ValueData: """{app}\bin\transom.exe"" ""%1"""; Tasks: rdpfiles

[Icons]
Name: "{group}\Transom"; Filename: "{app}\bin\transom.exe"
Name: "{group}\Uninstall Transom"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Transom"; Filename: "{app}\bin\transom.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\bin\transom.exe"; Description: "Launch Transom"; Flags: nowait postinstall skipifsilent

[Code]
// [InstallDelete]: an earlier Transom install in this folder.
function IsTransomUpgrade: Boolean;
begin
  Result := FileExists(ExpandConstant('{app}\bin\transom.exe'));
end;

// Delete Software\Classes\rdp (HKA) only if its open command is still Transom's: another
// client may have registered the rdp: scheme since.
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  RootKey: HKEY;
  Command, Exe: String;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  if IsAdminInstallMode then
    RootKey := HKLM
  else
    RootKey := HKCU;
  Exe := '"' + ExpandConstant('{app}\bin\transom.exe') + '"';
  if RegQueryStringValue(RootKey, 'Software\Classes\rdp\shell\open\command', '', Command) and
     (Pos(Lowercase(Exe), Lowercase(Command)) = 1) then
    RegDeleteKeyIncludingSubkeys(RootKey, 'Software\Classes\rdp');
end;
