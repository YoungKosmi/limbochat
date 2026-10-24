# Paczka ZIP dla znajomych: wtyczka z wbudowanym silnikiem + instalator.
. (Join-Path $PSScriptRoot 'wspolne.ps1')

$updates = Use-UpdateConfig
if (-not $updates) {
    Say 'UWAGA: autoaktualizacje nie są ustawione — znajomy będzie potrzebował' 'Yellow'
    Say 'każdej nowej wersji ręcznie. Ustaw je raz: ustaw-aktualizacje.bat' 'Yellow'
    Write-Host ''
}
Build-All
$out = Join-Path $Root 'LiMBOCHAT-OBS-dla-znajomego'
if (Test-Path $out) { Remove-Item $out -Recurse -Force }
New-Item -ItemType Directory -Path $out | Out-Null
Copy-Item (Join-Path $Root 'target\release\limbochat.dll') $out
Copy-Item (Join-Path $Root 'obs-plugin\dla-znajomego\ZAINSTALUJ.bat') $out
Copy-Item (Join-Path $Root 'obs-plugin\dla-znajomego\INSTRUKCJA.txt') $out
$zip = "$out.zip"
Compress-Archive -Path (Join-Path $out '*') -DestinationPath $zip -Force
Write-Host ''
Say 'Gotowe! Wyślij znajomemu ten plik:' 'Green'
Say $zip 'White'
if ($updates) { Say 'Kolejne wersje dostanie sam (wydaj-aktualizacje.bat).' 'Green' }
Write-Host ''
