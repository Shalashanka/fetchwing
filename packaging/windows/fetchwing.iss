; Fetchwing Windows installer (Inno Setup 6)
; Build: ISCC.exe packaging\windows\fetchwing.iss   (after `cargo build --release`)
;
; Deliberate contrasts with the JDownloader 2 installer (see docs/research/community-pain-points.md):
;  * no bundled offers / adware, no third-party anything
;  * per-user install by default (no admin prompt), machine-wide optional
;  * user data (downloads list, settings, accounts) is kept on uninstall unless you tick the box

#define AppName "Fetchwing"
#define AppVersion "0.1.0"
#define AppExe "fetchwing.exe"

[Setup]
AppId={{6E3D7B1A-5F0C-4C7B-9E21-F37C9A1D2B44}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Fetchwing research project
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\..\dist
OutputBaseFilename=Fetchwing-Setup-{#AppVersion}-x64
SetupIconFile=..\..\assets\fetchwing.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesEnvironment=yes
CloseApplications=yes
RestartApplications=no
LicenseFile=license.txt

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Shortcuts:"
Name: "autostart"; Description: "Start Fetchwing when I sign in to Windows"; GroupDescription: "Startup:"; Flags: unchecked
Name: "addtopath"; Description: "Add Fetchwing to PATH (use `fetchwing add <url>` from any terminal)"; GroupDescription: "Command line:"

[Files]
Source: "..\..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion isreadme
Source: "license.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autoprograms}\{#AppName} (web UI only)"; Filename: "{app}\{#AppExe}"; Parameters: "--headless"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#AppExe}"""; Flags: uninsdeletevalue; Tasks: autostart

[Run]
Filename: "{app}\{#AppExe}"; Description: "Launch {#AppName}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{cmd}"; Parameters: "/C taskkill /IM {#AppExe} /F"; Flags: runhidden; RunOnceId: "KillFetchwing"

[Code]
const
  EnvKey = 'Environment';

procedure AddToPath(Dir: string);
var
  Paths: string;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Paths) then Paths := '';
  if Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0 then exit;
  if (Paths <> '') and (Copy(Paths, Length(Paths), 1) <> ';') then Paths := Paths + ';';
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Paths + Dir);
end;

procedure RemoveFromPath(Dir: string);
var
  Paths: string;
  P: Integer;
begin
  if not RegQueryStringValue(HKCU, EnvKey, 'Path', Paths) then exit;
  P := Pos(';' + Uppercase(Dir), ';' + Uppercase(Paths));
  if P = 0 then exit;
  Delete(Paths, P - 1 + Ord(P = 1), Length(Dir) + 1);
  RegWriteExpandStringValue(HKCU, EnvKey, 'Path', Paths);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    AddToPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: string;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemoveFromPath(ExpandConstant('{app}'));
    DataDir := ExpandConstant('{localappdata}\Fetchwing');
    if DirExists(DataDir) then
      if MsgBox('Also delete your Fetchwing data (download list, settings, accounts) in' + #13#10 + DataDir + '?' + #13#10#13#10 + 'Downloaded files are NOT touched.', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(DataDir, True, True, True);
  end;
end;
