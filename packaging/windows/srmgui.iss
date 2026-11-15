; srmgui Windows installer script (Inno Setup 6)
; 编译示例：
;   ISCC.exe /DAppVersion=1.2.1 /DSourceDir="C:\path\to\srmgui" ^
;            /DSourceExe="C:\path\to\srmgui\target\release\srmgui.exe" ^
;            /DOutputDir="C:\path\to\srmgui\dist" packaging\windows\srmgui.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#ifndef SourceDir
  #define SourceDir "..\.."
#endif

#ifndef SourceExe
  #define SourceExe SourceDir + "\target\release\srmgui.exe"
#endif

#ifndef OutputDir
  #define OutputDir SourceDir + "\dist"
#endif

[Setup]
AppId={{B7A2F3C4-9D18-4E5A-A6C2-3F0D8E7B1A55}
AppName=srmgui
AppVersion={#AppVersion}
AppVerName=srmgui {#AppVersion}
AppPublisher=liujinpeng
AppPublisherURL=https://github.com/ljporljp/srmgui
AppSupportURL=https://github.com/ljporljp/srmgui
AppUpdatesURL=https://github.com/ljporljp/srmgui
DefaultDirName={autopf}\srmgui
DefaultGroupName=srmgui
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#OutputDir}
OutputBaseFilename=srmgui-{#AppVersion}-windows-x64-setup
SetupIconFile={#SourceDir}\assets\logo.ico
UninstallDisplayIcon={app}\srmgui.exe
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
LicenseFile={#SourceDir}\LICENSE

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\srmgui"; Filename: "{app}\srmgui.exe"
Name: "{group}\{cm:UninstallProgram,srmgui}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\srmgui"; Filename: "{app}\srmgui.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\srmgui.exe"; Description: "{cm:LaunchProgram,srmgui}"; Flags: nowait postinstall skipifsilent
