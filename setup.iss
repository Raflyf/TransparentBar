[Setup]
AppId={{C38347CB-7CD0-4D9F-BEE5-05204A3B269F}
AppName=TransparentBar
AppVersion=0.2.2
VersionInfoVersion=0.2.2.0
VersionInfoTextVersion=0.2.2
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
OutputBaseFilename=TransparentBar-setup-v0.2.2
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
Source: "dist\TransparentBar\transparentbar_bridge-0.2.1.dll"; DestDir: "{app}"; Flags: ignoreversion
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
