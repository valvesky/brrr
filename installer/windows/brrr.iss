; brrr installer for Windows 10 and 11 (Inno Setup 6.3 or newer).
;
; One setup file for x64 and ARM64 PCs: it installs the matching binary.
; brrr needs no administrator rights, so neither does the installer: it
; installs for the current user, under %LOCALAPPDATA%\Programs\brrr.
;
; Built by scripts/build-release.sh windows-installer, which passes:
;   /DAppVersion=2.0.0  /DDist=<folder with brrr-windows-x86_64.exe and
;   brrr-windows-aarch64.exe>  /DOutput=<folder for the setup file>

#ifndef AppVersion
  #error Pass /DAppVersion=x.y.z
#endif
#ifndef Dist
  #define Dist "..\..\dist"
#endif
#ifndef Output
  #define Output Dist
#endif

; Must match the value name src/windows/login.rs uses, so the menu's
; "Start at login" checkmark and this installer agree.
#define RunKey "Software\Microsoft\Windows\CurrentVersion\Run"
#define RunValue "brrr"
; The hidden window the tray app creates (src/windows/ui.rs). WM_CLOSE asks it
; to quit cleanly, which also puts back any power mode it changed.
#define TrayClass "brrr.Tray"

[Setup]
AppId={{70905226-2DCB-4D4A-A1EA-9C1F4C534A4F}
AppName=brrr
AppVersion={#AppVersion}
AppVerName=brrr {#AppVersion}
AppPublisher=brrr
AppPublisherURL=https://github.com/valvesky/brrr
AppSupportURL=https://github.com/valvesky/brrr/issues
AppUpdatesURL=https://github.com/valvesky/brrr/releases
AppComments=Full power when you open something heavy. Quiet again when you close it.
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\brrr
UsePreviousAppDir=no
DisableDirPage=auto
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir={#Output}
OutputBaseFilename=brrr-windows-setup
SetupIconFile=..\..\assets\icon\brrr.ico
UninstallDisplayIcon={app}\brrr.ico
UninstallDisplayName=brrr
LicenseFile=..\..\LICENSE
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
; The app is closed by the code below, which lets it restore the power mode;
; the Restart Manager would only find it after the files are already in use.
CloseApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startup"; Description: "Start brrr when I sign in"; GroupDescription: "Startup:"

[Files]
Source: "{#Dist}\brrr-windows-x86_64.exe"; DestDir: "{app}"; DestName: "brrr.exe"; Check: not IsArm64; Flags: ignoreversion
Source: "{#Dist}\brrr-windows-aarch64.exe"; DestDir: "{app}"; DestName: "brrr.exe"; Check: IsArm64; Flags: ignoreversion
Source: "..\..\assets\icon\brrr.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\brrr"; Filename: "{app}\brrr.exe"; IconFilename: "{app}\brrr.ico"; Comment: "Full power when something heavy is open"

[Registry]
; The same entry the app writes from its menu, quoted the same way.
Root: HKCU; Subkey: "{#RunKey}"; ValueType: string; ValueName: "{#RunValue}"; ValueData: """{app}\brrr.exe"""; Tasks: startup

[Run]
Filename: "{app}\brrr.exe"; Description: "Start brrr now"; Flags: postinstall nowait skipifsilent

[InstallDelete]
Type: files; Name: "{app}\omapower.exe"
Type: files; Name: "{app}\omapower.ico"

[Code]
const
  WM_CLOSE = $0010;

var
  OldDir: String;

{ Ask a running tray app to quit and wait, so it can restore the power mode
  before its files are replaced or removed. }
procedure CloseByClass(const ClassName: String);
var
  Window: HWND;
  Waited: Integer;
begin
  Window := FindWindowByClassName(ClassName);
  if Window = 0 then
    Exit;
  PostMessage(Window, WM_CLOSE, 0, 0);
  Waited := 0;
  while (FindWindowByClassName(ClassName) <> 0) and (Waited < 10000) do
  begin
    Sleep(200);
    Waited := Waited + 200;
  end;
end;

procedure CloseApp();
begin
  CloseByClass('{#TrayClass}');
  CloseByClass('Omapower.Tray');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  OldDir := '';
  RegQueryStringValue(HKCU,
    'Software\Microsoft\Windows\CurrentVersion\Uninstall\{70905226-2DCB-4D4A-A1EA-9C1F4C534A4F}_is1',
    'InstallLocation', OldDir);
  CloseApp();
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    RegDeleteValue(HKEY_CURRENT_USER, '{#RunKey}', 'Omapower');
    DeleteFile(ExpandConstant('{autoprograms}\Omapower.lnk'));
    if (OldDir <> '') and (CompareText(AddBackslash(OldDir), AddBackslash(ExpandConstant('{app}'))) <> 0) then
    begin
      DeleteFile(AddBackslash(OldDir) + 'omapower.exe');
      DeleteFile(AddBackslash(OldDir) + 'omapower.ico');
    end;
  end;
end;

{ Remove the login entry whether the installer or the app's menu created it.
  Settings in %APPDATA%\brrr are kept, like on the other platforms. The
  pre-rename Run value is removed too, because this is the same install. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    CloseApp();
    RegDeleteValue(HKEY_CURRENT_USER, '{#RunKey}', '{#RunValue}');
    RegDeleteValue(HKEY_CURRENT_USER, '{#RunKey}', 'Omapower');
  end;
end;
