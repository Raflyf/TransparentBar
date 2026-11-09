[Setup]
AppId={{C38347CB-7CD0-4D9F-BEE5-05204A3B269F}
AppName=TransparentBar
AppVersion=0.2.4
VersionInfoVersion=0.2.4.0
VersionInfoTextVersion=0.2.4
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
OutputBaseFilename=TransparentBar-setup-v0.2.4
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupIconFile=assets\transparentbar.ico
UninstallDisplayIcon={app}\transparentbar.exe
AppMutex=Local\TransparentBar.SingleInstance
CloseApplications=no
RestartApplications=no
LicenseFile=LICENSE.md

[Files]
Source: "dist\TransparentBar\transparentbar.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "dist\TransparentBar\transparentbar_bridge-0.2.4.dll"; DestDir: "{app}"; Flags: ignoreversion
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
function PostMessage(hWnd: HWND; Msg: Cardinal; wParam: LongInt; lParam: LongInt): Boolean;
external 'PostMessageW@user32.dll stdcall';

const
  WM_CLOSE = $0010;

procedure CloseRunningInstance();
var
  H: HWND;
  I: Integer;
begin
  H := FindWindowByClassName('TransparentBar.Window');
  if H <> 0 then
  begin
    PostMessage(H, WM_CLOSE, 0, 0);
    I := 0;
    while (FindWindowByClassName('TransparentBar.Window') <> 0) and (I < 50) do
    begin
      Sleep(100);
      I := I + 1;
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  AppDir, BridgeDll, OldDll: String;
begin
  if CurStep = ssInstall then
  begin
    AppDir := ExpandConstant('{app}');
    BridgeDll := AppDir + '\transparentbar_bridge-0.2.4.dll';
    OldDll := BridgeDll + '.old';
    if FileExists(BridgeDll) then
    begin
      DeleteFile(OldDll);
      if not DeleteFile(BridgeDll) then
      begin
        RenameFile(BridgeDll, OldDll);
      end;
    end;
  end;
end;

function InitializeSetup(): Boolean;
begin
  CloseRunningInstance();
  Result := True;
end;

function InitializeUninstall(): Boolean;
begin
  CloseRunningInstance();
  Result := True;
end;
