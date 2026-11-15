; srmgui Windows installer script (Inno Setup 6)
; 编译示例：
;   ISCC.exe /DAppVersion=1.3.0 /DSourceDir="C:\path\to\srmgui" ^
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
; 覆盖安装时由 Restart Manager 检测占用文件的进程并要求关闭
CloseApplications=yes
RestartApplications=no

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

[Code]
const
  AppExeName = 'srmgui.exe';

{ 检测 srmgui 是否仍在运行，避免覆盖写入时因文件被占用而失败。 }
function IsAppRunning(): Boolean;
var
  ResultCode: Integer;
  TempFile: String;
  Output: AnsiString;
begin
  Result := False;
  TempFile := ExpandConstant('{tmp}\srmgui-tasklist.txt');
  if Exec(ExpandConstant('{cmd}'),
    '/C tasklist /FI "IMAGENAME eq ' + AppExeName + '" /NH > "' + TempFile + '"',
    '', SW_HIDE, ewWaitUntilTerminated, ResultCode) then
  begin
    if LoadStringFromFile(TempFile, Output) then
      Result := Pos(AppExeName, String(Output)) > 0;
    DeleteFile(TempFile);
  end;
end;

{ 写入文件前确认程序已退出；用户选择取消时中止安装。 }
function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
  while IsAppRunning() do
  begin
    if MsgBox('srmgui is still running. Please close it, then click Retry to continue.'
      + #13#10 + #13#10 + 'Click Cancel to abort the installation.',
      mbError, MB_RETRYCANCEL) <> IDRETRY then
    begin
      Result := 'srmgui is still running. Installation was cancelled to avoid file write failures.';
      Exit;
    end;
  end;
end;
