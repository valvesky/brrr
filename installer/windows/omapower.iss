; Omapower installer for Windows 10 and 11 (Inno Setup 6.3 or newer).
;
; One setup file for x64 and ARM64 PCs: it installs the matching binary.
; Omapower needs no administrator rights, so neither does the installer: it
; installs for the current user, under %LOCALAPPDATA%\Programs\Omapower.
;
; Built by scripts/build-release.sh windows-installer, which passes:
;   /DAppVersion=2.0.0  /DDist=<folder with omapower-windows-x86_64.exe and
;   omapower-windows-aarch64.exe>  /DOutput=<folder for the setup file>

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
#define RunValue "Omapower"
; The hidden window the tray app creates (src/windows/ui.rs). WM_CLOSE asks it
; to quit cleanly, which also puts back any power mode it changed.
#define TrayClass "Omapower.Tray"

[Setup]
AppId={{70905226-2DCB-4D4A-A1EA-9C1F4C534A4F}
AppName=Omapower
AppVersion={#AppVersion}
AppVerName=Omapower {#AppVersion}
AppPublisher=Omapower
AppPublisherURL=https://github.com/valvesky/omapower
AppSupportURL=https://github.com/valvesky/omapower/issues
AppUpdatesURL=https://github.com/valvesky/omapower/releases
AppComments=Full power while you compile. Back to normal when you're done.
VersionInfoVersion={#AppVersion}
PrivilegesRequired=lowest
DefaultDirName={autopf}\Omapower
DisableDirPage=auto
DisableProgramGroupPage=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir={#Output}
OutputBaseFilename=omapower-windows-setup
SetupIconFile=..\..\assets\icon\omapower.ico
UninstallDisplayIcon={app}\omapower.ico
UninstallDisplayName=Omapower
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
Name: "startup"; Description: "Start Omapower when I sign in"; GroupDescription: "Startup:"

[Files]
Source: "{#Dist}\omapower-windows-x86_64.exe"; DestDir: "{app}"; DestName: "omapower.exe"; Check: not IsArm64; Flags: ignoreversion
Source: "{#Dist}\omapower-windows-aarch64.exe"; DestDir: "{app}"; DestName: "omapower.exe"; Check: IsArm64; Flags: ignoreversion
Source: "..\..\assets\icon\omapower.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Omapower"; Filename: "{app}\omapower.exe"; IconFilename: "{app}\omapower.ico"; Comment: "Full power while you compile"

[Registry]
; The same entry the app writes from its menu, quoted the same way.
Root: HKCU; Subkey: "{#RunKey}"; ValueType: string; ValueName: "{#RunValue}"; ValueData: """{app}\omapower.exe"""; Tasks: startup

[Run]
Filename: "{app}\omapower.exe"; Description: "Start Omapower now"; Flags: postinstall nowait skipifsilent

[Code]
const
  WM_CLOSE = $0010;

{ Ask a running Omapower to quit and wait for it, so it can restore the power
  mode it holds before its files are replaced or removed. }
procedure CloseOmapower();
var
  Window: HWND;
  Waited: Integer;
begin
  Window := FindWindowByClassName('{#TrayClass}');
  if Window = 0 then
    Exit;
  PostMessage(Window, WM_CLOSE, 0, 0);
  Waited := 0;
  while (FindWindowByClassName('{#TrayClass}') <> 0) and (Waited < 10000) do
  begin
    Sleep(200);
    Waited := Waited + 200;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  CloseOmapower();
  Result := '';
end;

{ Remove the login entry whether the installer or the app's menu created it.
  Settings in %APPDATA%\omapower are kept, like on the other platforms. }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
  begin
    CloseOmapower();
    RegDeleteValue(HKEY_CURRENT_USER, '{#RunKey}', '{#RunValue}');
  end;
end;
