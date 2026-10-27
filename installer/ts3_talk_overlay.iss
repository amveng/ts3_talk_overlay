; Installer for the TS3 plugin. Build with installer\build.ps1.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{8F60C83E-4940-439E-A16E-4BCBC668B99C}
AppName=TS Talk Overlay
AppVersion={#AppVersion}
AppPublisher=TakeChance
DefaultDirName={code:TsPluginsDir}
AppendDefaultDirName=no
DirExistsWarning=no
DisableProgramGroupPage=yes
UninstallFilesDir={commonappdata}\ts3_talk_overlay
UninstallDisplayName=TS Talk Overlay (плагин TeamSpeak 3)
PrivilegesRequired=admin
UsedUserAreasWarning=no
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\dist
OutputBaseFilename=ts3_talk_overlay_setup_{#AppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "ru"; MessagesFile: "compiler:Languages\Russian.isl"

[Messages]
SelectDirLabel3=Плагин будет установлен в папку плагинов TeamSpeak 3. Папка найдена автоматически; измените её, только если TeamSpeak хранит настройки в другом месте.
FinishedLabel=Плагин установлен. Запустите TeamSpeak 3 и включите плагин «TS Talk Overlay» в Настройки → Плагины. Внешний вид меняется кнопкой «Настройки».

[Files]
Source: "..\target\release\ts3_talk_overlay.dll"; DestDir: "{app}"; DestName: "ts3_talk_overlay_win64.dll"; Flags: ignoreversion
Source: "..\Инструкция по установке.txt"; DestDir: "{app}"; DestName: "ts3_talk_overlay — инструкция.txt"; Flags: ignoreversion

[UninstallDelete]
Type: files; Name: "{app}\ts3_talk_overlay.ini"
Type: files; Name: "{app}\ts3_talk_overlay.log"

[Code]
const
  TsKey = 'Software\TeamSpeak 3 Client';

{ TS stores its config next to the program when ConfigLocation = 1, otherwise in %APPDATA%. }
function TsPluginsDir(Param: String): String;
var
  Dir, Loc: String;
begin
  Result := ExpandConstant('{userappdata}\TS3Client\plugins');
  if RegQueryStringValue(HKCU, TsKey, '', Dir) then
    RegQueryStringValue(HKCU, TsKey, 'ConfigLocation', Loc)
  else if RegQueryStringValue(HKLM, TsKey, '', Dir) then
    RegQueryStringValue(HKLM, TsKey, 'ConfigLocation', Loc);
  if (Dir <> '') and (Loc = '1') then
    Result := AddBackslash(Dir) + 'config\plugins';
end;

function IsTsRunning: Boolean;
var
  Locator, Wmi, Procs: Variant;
begin
  Locator := CreateOleObject('WbemScripting.SWbemLocator');
  Wmi := Locator.ConnectServer('.', 'root\CIMV2');
  Procs := Wmi.ExecQuery('SELECT ProcessId FROM Win32_Process WHERE Name = ''ts3client_win64.exe''');
  Result := Procs.Count > 0;
end;

function WaitForTsClosed: Boolean;
begin
  Result := True;
  while IsTsRunning do
    if MsgBox('Закройте TeamSpeak 3 и нажмите «Повтор».', mbError, MB_RETRYCANCEL) = IDCANCEL then
    begin
      Result := False;
      Exit;
    end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  if WaitForTsClosed then
    Result := ''
  else
    Result := 'TeamSpeak 3 запущен.';
end;

function InitializeUninstall: Boolean;
begin
  Result := WaitForTsClosed;
end;

[Run]
Filename: "{app}\ts3_talk_overlay — инструкция.txt"; Description: "Открыть инструкцию"; Flags: postinstall shellexec skipifsilent
Filename: "{app}"; Description: "Открыть папку плагинов"; Flags: postinstall shellexec skipifsilent unchecked
