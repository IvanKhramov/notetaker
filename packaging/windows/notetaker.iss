; Inno Setup 6, current user only. CI supplies the audited ZIP staging directory.
#ifndef AppVersion
  #define AppVersion "0.7.0"
#endif
#ifndef PayloadDir
  #define PayloadDir "..\..\target\release"
#endif

[Setup]
AppId={{9A1B3B55-9F48-4D83-A8E7-706D71F4F5D9}
AppName=notetaker
AppVersion={#AppVersion}
AppVerName=notetaker {#AppVersion}
AppPublisher=Ivan Khramov
AppPublisherURL=https://github.com/IvanKhramov/notetaker
AppSupportURL=https://github.com/IvanKhramov/notetaker/issues
AppUpdatesURL=https://github.com/IvanKhramov/notetaker/releases
DefaultDirName={autopf}\notetaker
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
SetupIconFile=..\..\crates\app\assets\notetaker.ico
UninstallDisplayIcon={app}\notetaker-app.exe
UninstallDisplayName=notetaker
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
OutputDir=..\..\target\installer
OutputBaseFilename=notetaker-v{#AppVersion}-windows-x64-setup
; Our IPC checks recording/finalization before closing a tray app.
CloseApplications=no
RestartApplications=no

[Languages]
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
ru.Autostart=Запускать notetaker при входе в Windows
en.Autostart=Start notetaker when I sign in to Windows
ru.StopFailed=Не удалось закрыть notetaker. Завершите запись и чистовой проход, закройте приложение и повторите попытку.
en.StopFailed=Could not close notetaker. Finish recording and finalization, close the app and try again.

[Tasks]
Name: "autostart"; Description: "{cm:Autostart}"; Flags: unchecked
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "{#PayloadDir}\notetaker-app.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\notetaker.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\notetaker-asr.exe"; DestDir: "{app}"; Flags: ignoreversion
; Optional custom payload only; the standard installer downloads this worker on demand.
Source: "{#PayloadDir}\notetaker-embed.exe"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#PayloadDir}\*.dll"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\notetaker"; Filename: "{app}\notetaker-app.exe"
Name: "{autodesktop}\notetaker"; Filename: "{app}\notetaker-app.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; \
  ValueName: "notetaker"; ValueData: """{app}\notetaker-app.exe"""; Tasks: autostart
; Remove on uninstall even if enabled later in the app settings.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; \
  ValueName: "notetaker"; Flags: uninsdeletevalue dontcreatekey

[Run]
Filename: "{app}\notetaker-app.exe"; Description: "{cm:LaunchProgram,notetaker}"; Flags: nowait postinstall skipifsilent

[Code]
function StopApp(const Exe: String): Boolean;
var
  ExitCode: Integer;
begin
  Result := Exec(Exe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, ExitCode);
  if Result then
  begin
    Log(Format('notetaker --quit exited with code %d.', [ExitCode]));
    // Exit code 0 includes the case where no instance is running.
    Result := ExitCode = 0;
  end
  else
    Log(Format('Could not run notetaker --quit: %d (%s).',
      [ExitCode, SysErrorMessage(ExitCode)]));
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  // Use the new executable, including on first install over a portable copy.
  ExtractTemporaryFile('notetaker-app.exe');
  if StopApp(ExpandConstant('{tmp}\notetaker-app.exe')) then
    Result := ''
  else
    Result := CustomMessage('StopFailed');
end;

function InitializeUninstall(): Boolean;
begin
  Result := StopApp(ExpandConstant('{app}\notetaker-app.exe'));
  if not Result and not UninstallSilent then
    MsgBox(CustomMessage('StopFailed'), mbError, MB_OK);
end;
