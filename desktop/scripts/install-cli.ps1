$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')
$source = Join-Path (Get-Location) 'src-tauri\target\release\pow.exe'
if (-not (Test-Path $source)) { throw "Missing $source; run 'npm run tauri -- build --no-bundle' first." }
$installDir = Join-Path $env:LOCALAPPDATA 'Pow\bin'
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
Copy-Item -Force $source (Join-Path $installDir 'pow.exe')
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $installDir) {
  [Environment]::SetEnvironmentVariable('Path', "$userPath;$installDir", 'User')
  Write-Output 'Open a new terminal for the updated PATH to take effect.'
}
Write-Output "Installed $installDir\pow.exe. Run: pow"
