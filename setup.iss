[Setup]
AppId={{C38347CB-7CD0-4D9F-BEE5-05204A3B269F}
AppName=TransparentBar
AppVersion=0.2.3
VersionInfoVersion=0.2.3.0
VersionInfoTextVersion=0.2.3
VersionInfoDescription=TransparentBar Setup
VersionInfoCopyright=Copyright 2026 Rafly Firmansyah
AppPublisher=Rafly Firmansyah
AppPublisherURL=https://github.com/Raflyf/transparentbar
DefaultDirName={localappdata}\Programs\TransparentBar
DefaultGroupName=TransparentBar
PrivilegesRequired=lowest
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
MinVersion=10.0
OutputDir=dist
OutputBaseFilename=TransparentBar-setup-v0.2.3
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupIconFile=assets\transparentbar.ico
UninstallDisplayIcon={app}\transparentbar.exe
AppMutex=Local\TransparentBar.SingleInstance
CloseApplications=yes
RestartApplications=no
LicenseFile=LICENSE.md

[Files]
Source: "dist\TransparentBar\transparentbar.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "dist\TransparentBar\transparentbar_bridge-0.2.3.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "LICENSE.md"; DestDir: "{app}"
Source: "README.md"; DestDir: "{app}"
Source: "dist\TransparentBar\BUILD.txt"; DestDir: "{app}"; Flags: ignoreversion

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked
Name: "startup"; Description: "Run at Windows sign-in"; Flags: unchecked

[Icons]
Name: "{group}\TransparentBar"; Filename: "{app}\transparentbar.exe"
Name: "{userdesktop}\TransparentBar"; Filename: "{app}\transparentbar.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "TransparentBar"; ValueData: """{app}\transparentbar.exe"""; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\transparentbar.exe"; Description: "Launch TransparentBar"; Flags: nowait postinstall skipifsilent

[Code]
function FindWindow(LpClassName, LpWindowName: String): HWND;
  external 'user32.dll stdcall';

{ Close the running instance gracefully so the app restores the saved
  taskbar appearance and releases its files before Setup copies over them.
  Bounded wait; CloseApplications stays as a fallback if the close fails. }
function InitializeSetup(): Boolean;
var
  H: HWND;
  I: Integer;
begin
  H := FindWindow('TransparentBar.Window', '');
  if H <> 0 then
  begin
    PostMessage(H, WM_CLOSE, 0, 0);
    I := 0;
    while (FindWindow('TransparentBar.Window', '') <> 0) and (I < 50) do
    begin
      Delay(100);
      I := I + 1;
    end;
  end;
  Result := True;
end;
