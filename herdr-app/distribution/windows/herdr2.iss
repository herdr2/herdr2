; Inno Setup 6 script for the unified Herdr2 Windows installer (App + h2 Core).
; Installs per-user with zero admin prompts into:
;   - %LOCALAPPDATA%\Programs\herdr2  (herdr2.exe desktop app)
;   - %LOCALAPPDATA%\Programs\h2      (h2.exe + herdr2-core.exe runtime)
;
; Build example:
;   iscc /DMyAppVersion=0.2.0 /DAppStageDir=dist\stage-app /DCoreStageDir=dist\stage-core /DOutputDir=dist\installers herdr-app\distribution\windows\herdr2.iss

#ifndef MyAppVersion
  #define MyAppVersion "2.0.12"
#endif
#ifndef AppStageDir
  #define AppStageDir "..\..\dist\stage-app"
#endif
#ifndef CoreStageDir
  #define CoreStageDir "..\..\dist\stage-core"
#endif
#ifndef OutputDir
  #define OutputDir "..\..\dist\installers"
#endif

[Setup]
AppId={{9C2E7B14-64B5-4C9B-B788-58E8C8E0922F}
AppName=Herdr2
AppVersion={#MyAppVersion}
AppVerName=Herdr2 {#MyAppVersion}
AppPublisher=Herdr2
AppPublisherURL=https://github.com/herdr2/herdr2
AppSupportURL=https://github.com/herdr2/herdr2/issues
DefaultDirName={localappdata}\Programs\herdr2
DisableDirPage=no
DefaultGroupName=Herdr2
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutputDir}
OutputBaseFilename=Herdr2-Setup-{#MyAppVersion}-windows-x86_64
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
SetupIconFile=..\..\assets\icons\herdr-app.ico
UninstallDisplayIcon={app}\herdr2.exe

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &Desktop shortcut"; GroupDescription: "Additional shortcuts:"
Name: "addpath"; Description: "Add herdr2 and h2 CLI to user PATH (recommended)"; GroupDescription: "Command-line tools:"

[Files]
; Desktop app
Source: "{#AppStageDir}\herdr2.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#AppStageDir}\herdr2.svg"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#AppStageDir}\herdr2.ico"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#AppStageDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#AppStageDir}\THIRD-PARTY-NOTICES"; DestDir: "{app}"; Flags: ignoreversion

; Bundled h2 core runtime in %LOCALAPPDATA%\Programs\h2 (zero collision with herdr2.exe)
Source: "{#CoreStageDir}\herdr2-core.exe"; DestDir: "{localappdata}\Programs\h2"; Flags: ignoreversion
Source: "{#CoreStageDir}\h2.exe"; DestDir: "{localappdata}\Programs\h2"; Flags: ignoreversion
Source: "{#CoreStageDir}\THIRD-PARTY-NOTICES"; DestDir: "{localappdata}\Programs\h2"; Flags: ignoreversion skipifsourcedoesntexist
Source: "{#CoreStageDir}\conpty\*"; DestDir: "{localappdata}\Programs\h2\conpty"; Flags: ignoreversion recursesubdirs createallsubdirs skipifsourcedoesntexist

[Icons]
Name: "{autoprograms}\Herdr2"; Filename: "{app}\herdr2.exe"; WorkingDir: "{%USERPROFILE}"; IconFilename: "{app}\herdr2.exe"; IconIndex: 0
Name: "{autodesktop}\Herdr2"; Filename: "{app}\herdr2.exe"; WorkingDir: "{%USERPROFILE}"; IconFilename: "{app}\herdr2.exe"; IconIndex: 0; Tasks: desktopicon

[Run]
Filename: "{app}\herdr2.exe"; Description: "Launch Herdr2 now"; Flags: nowait postinstall skipifsilent

[Code]
procedure EnsureUserPathEntry(const Dir: string);
var
  CurPath, CleanDir, LowerPath, LowerDir: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', CurPath) then
    CurPath := '';
  CleanDir := RemoveBackslashUnlessRoot(Dir);
  LowerPath := ';' + Lowercase(CurPath) + ';';
  LowerDir := ';' + Lowercase(CleanDir) + ';';
  if Pos(LowerDir, LowerPath) = 0 then
  begin
    if (Length(CurPath) > 0) and (CurPath[Length(CurPath)] <> ';') then
      CurPath := CurPath + ';';
    CurPath := CurPath + CleanDir;
    RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', CurPath);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addpath') then
  begin
    EnsureUserPathEntry(ExpandConstant('{app}'));
    EnsureUserPathEntry(ExpandConstant('{localappdata}\Programs\h2'));
  end;
end;
