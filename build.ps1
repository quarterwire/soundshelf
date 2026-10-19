# Builds the release app, puts SoundShelf.exe in this folder, and adds a Start menu shortcut
# so it shows up when you search "SoundShelf" in the Windows search bar.
#   .\build.ps1               build + shortcut
#   .\build.ps1 -NoShortcut   build only
param([switch]$NoShortcut)
$ErrorActionPreference = 'Stop'
$App = 'SoundShelf'
$Crate = 'soundshelf'
Set-Location $PSScriptRoot

# Fresh PATH from the registry, so cargo and MinGW are found even in an old terminal.
$env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' + [Environment]::GetEnvironmentVariable('Path', 'User') + ";$env:USERPROFILE\.cargo\bin"

cargo build --release
if ($LASTEXITCODE) { exit $LASTEXITCODE }

$exe = Join-Path $PSScriptRoot "$App.exe"
$running = Get-Process $App -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe }
if ($running) {
    Write-Host "Closing the running $App so it can be updated..."
    $running | Stop-Process
    $running | Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}
# Antivirus or Explorer can hold a just-written exe for a moment; retry briefly.
for ($try = 1; ; $try++) {
    try { Copy-Item "target\release\$Crate.exe" $exe -Force; break }
    catch { if ($try -ge 10) { throw "Couldn't replace $exe (is it open?): $_" }; Start-Sleep -Milliseconds 500 }
}
Write-Host "Built $exe"

if (-not $NoShortcut) {
    $lnk = Join-Path ([Environment]::GetFolderPath('Programs')) "$App.lnk"
    $shell = New-Object -ComObject WScript.Shell
    $s = $shell.CreateShortcut($lnk)
    $s.TargetPath = $exe
    $s.WorkingDirectory = $PSScriptRoot
    $s.IconLocation = "$exe,0"
    $s.Description = 'Fast audio library for songs and SFX'
    $s.Save()
    Write-Host "Start menu shortcut: $lnk (search '$App' in Windows)"
}
