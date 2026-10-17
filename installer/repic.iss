; Instalador do RePic (Inno Setup 6).
; Compilar: iscc /DMyAppVersion=0.3.0 /DFfmpegExe=caminho\ffmpeg.exe installer\repic.iss
; Sai em dist\RePic-Setup.exe.

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif
#ifndef FfmpegExe
  #define FfmpegExe "..\dist\ffmpeg.exe"
#endif

[Setup]
AppId={{A8C3F1E2-5B7D-4E9A-9C21-7F3D2B6E8A41}
AppName=RePic
AppVersion={#MyAppVersion}
AppVerName=RePic {#MyAppVersion}
AppPublisher=FacincaniTech
AppPublisherURL=https://facincanitech.github.io/repic/
AppSupportURL=https://github.com/facincanitech/repic
; Instala só pro usuário: não pede administrador.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\RePic
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=yes
OutputDir=..\dist
OutputBaseFilename=RePic-Setup
SetupIconFile=..\assets\repic.ico
UninstallDisplayIcon={app}\RePic.exe
UninstallDisplayName=RePic
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Atualizar com o RePic aberto: fecha ele antes.
CloseApplications=force
RestartApplications=no
ShowLanguageDialog=auto

[Languages]
Name: "pt"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"

[CustomMessages]
pt.AutoStart=Iniciar o RePic com o Windows (abre na bandeja, já gravando)
en.AutoStart=Start RePic with Windows (opens in the tray, already recording)
es.AutoStart=Iniciar RePic con Windows (se abre en la bandeja, ya grabando)

[Tasks]
Name: "autostart"; Description: "{cm:AutoStart}"
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "..\target\release\RePic.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#FfmpegExe}"; DestDir: "{app}"; DestName: "ffmpeg.exe"; Flags: ignoreversion
Source: "..\dist-files\LEIA-ME.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\RePic"; Filename: "{app}\RePic.exe"
Name: "{autodesktop}\RePic"; Filename: "{app}\RePic.exe"; Tasks: desktopicon

[Registry]
; Mesmo valor que o botão "Iniciar com o Windows" do app usa.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "RePic"; ValueData: """{app}\RePic.exe"" --tray"; Tasks: autostart

[Run]
Filename: "{app}\RePic.exe"; Description: "{cm:LaunchProgram,RePic}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Fecha o RePic (e o ffmpeg dele) antes de apagar os arquivos.
Filename: "{sys}\taskkill.exe"; Parameters: "/F /T /IM RePic.exe"; Flags: runhidden; RunOnceId: "CloseRePic"

[UninstallDelete]
Type: filesandordirs; Name: "{app}"

[Code]
procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
begin
  // Atualização: garante que a versão antiga não está rodando.
  if CurStep = ssInstall then
    Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /T /IM RePic.exe', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // O app pode ter ligado o "iniciar com o Windows" sozinho; tira na desinstalação.
  // Clipes (Vídeos\RePic) e configurações ficam.
  if CurUninstallStep = usPostUninstall then
    RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'RePic');
end;
