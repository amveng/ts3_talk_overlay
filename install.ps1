# Builds the plugin and copies it into the TeamSpeak plugins folder. Close TeamSpeak first.
$ErrorActionPreference = "Stop"
$plugins = "D:\programs\TS\config\plugins"
Set-Location $PSScriptRoot
cargo build --release
Copy-Item "target\release\ts3_talk_overlay.dll" "$plugins\ts3_talk_overlay_win64.dll" -Force
Write-Host "Installed to $plugins"
