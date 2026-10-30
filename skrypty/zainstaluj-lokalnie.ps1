# Buduje i instaluje wtyczkę na TYM komputerze (OBS musi być zamknięty).
. (Join-Path $PSScriptRoot 'wspolne.ps1')

if (Get-Process obs64 -ErrorAction SilentlyContinue) { Fail 'OBS jest uruchomiony. Zamknij OBS i uruchom ten plik ponownie.' }
$updates = Use-UpdateConfig
Build-All
$version = Get-EngineVersion

# Silnik: od razu do folderu silnika (żeby nowa kompilacja zastąpiła tę samą wersję).
$engineDir = Join-Path $env:LOCALAPPDATA 'LiMBOCHAT\engine'
New-Item -ItemType Directory -Force -Path $engineDir | Out-Null
$name = "limbochat-engine-$version.exe"
Copy-Item (Join-Path $Root 'target\release\limbochat-engine.exe') (Join-Path $engineDir $name) -Force
[IO.File]::WriteAllText((Join-Path $engineDir 'current.txt'), $name, $Utf8)

# Wtyczka OBS (wymaga uprawnień administratora — folder ProgramData).
$dest = Join-Path $env:ProgramData 'obs-studio\plugins\limbochat\bin\64bit'
$dll = Join-Path $Root 'target\release\limbochat.dll'
try {
    New-Item -ItemType Directory -Force -Path $dest, (Join-Path $env:ProgramData 'obs-studio\plugins\limbochat\data') | Out-Null
    Copy-Item $dll (Join-Path $dest 'limbochat.dll') -Force
} catch {
    Say 'Potrzebne uprawnienia administratora — zatwierdź okno Windows...' 'Yellow'
    $cmd = "New-Item -ItemType Directory -Force -Path '$dest' | Out-Null; Copy-Item '$dll' '$dest\limbochat.dll' -Force"
    $p = Start-Process powershell -Verb RunAs -Wait -PassThru -ArgumentList '-NoProfile', '-Command', $cmd
    if ($p.ExitCode -ne 0) { Fail 'Nie udało się skopiować wtyczki do folderu OBS.' }
}

Write-Host ''
Say "Gotowe! LiMBOCHAT $version zainstalowany." 'Green'
if (-not $updates) { Say 'Autoaktualizacje nie są ustawione (ustaw-aktualizacje.bat).' 'Yellow' }
Write-Host ''
Say 'W OBS (jednorazowo): Doki → Niestandardowe doki przeglądarki…'
Say 'Nazwa: LiMBOCHAT    Adres URL: http://127.0.0.1:47250'
Say 'Overlay na stream: Źródła → + → Przeglądarka → http://127.0.0.1:47250/overlay'
Write-Host ''
