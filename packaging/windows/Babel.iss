; Build-time paths and version are supplied by build.py, never user configuration.
#ifndef BabelVersion
  #error BabelVersion is required
#endif
#ifndef PayloadDir
  #error PayloadDir is required
#endif
#ifndef OutputFolder
  #error OutputFolder is required
#endif
#ifndef TargetArch
  #error TargetArch is required
#endif

[Setup]
AppId={{FB7BA988-D38F-48E3-883B-6C7AE36297A5}
AppName=Babel
AppVersion={#BabelVersion}
AppPublisher=Babel contributors
DefaultDirName={autopf}\Babel
DefaultGroupName=Babel
DisableProgramGroupPage=yes
PrivilegesRequired=admin
#if TargetArch == "x64"
ArchitecturesAllowed=x64os
ArchitecturesInstallIn64BitMode=x64os
#else
ArchitecturesAllowed=arm64
ArchitecturesInstallIn64BitMode=arm64
#endif
MinVersion=10.0.19041
OutputDir={#OutputFolder}
OutputBaseFilename=Babel-{#BabelVersion}-windows-{#TargetArch}-development
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\babel-tray.exe
LicenseFile={#PayloadDir}\LICENSE-MIT.txt
InfoBeforeFile={#PayloadDir}\INSTALLATION.txt
CloseApplications=yes
RestartApplications=no
SetupLogging=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Files]
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\Babel"; Filename: "{app}\babel-tray.exe"; WorkingDir: "{app}"
Name: "{group}\Driver installation guide"; Filename: "{app}\INSTALLATION.txt"
Name: "{group}\Uninstall Babel"; Filename: "{uninstallexe}"

; Drivers and the app never start during installation. This development package
; contains an unsigned catalog; kernel loading still requires Microsoft signing.
; No Run entry, default-device change, certificate install or login task.
[Code]
function InitializeUninstall(): Boolean;
var
  ExitCode: Integer;
  Helper: String;
begin
  Result := False;
  Helper := ExpandConstant('{app}\drivers\windows\babel-driver-installer.exe');
  if not FileExists(Helper) then begin
    MsgBox('The Babel driver removal helper is missing. Repair this installation before removing it.', mbError, MB_OK);
    Exit;
  end;
  { InitializeUninstall runs before the confirmation prompt. Check only; never
    remove a device here. Keep all files when explicit driver removal is needed. }
  if not Exec(Helper, 'check-absent', '', SW_HIDE, ewWaitUntilTerminated, ExitCode) then begin
    MsgBox('Could not check the Babel audio driver installation.', mbError, MB_OK);
    Exit;
  end;
  if ExitCode <> 0 then begin
    MsgBox('Remove the Babel audio driver first. Close audio applications, run drivers\windows\uninstall.ps1 from an Administrator PowerShell, and reboot if requested. The application files have been preserved.', mbError, MB_OK);
    Exit;
  end;
  Result := True;
end;
