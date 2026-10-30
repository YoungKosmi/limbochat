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

# Twoje klucze LimboAI — tylko w tej prywatnej paczce (nigdy na GitHubie).
Write-Host ''
Say 'Dołączyć Twoje klucze LimboAI (Gemini / Groq / własny model)?' 'White'
Say 'Znajomy nie będzie musiał zakładać kont — LimboAI zadziała od razu na Twoich kluczach.'
Say 'Uwaga: kto ma tę paczkę, ten używa Twoich limitów. Wysyłaj ją tylko prywatnie.' 'Yellow'
$answer = Read-Host '  Dołączyć? (t/n)'
if ($answer -match '^[tTyY]') {
    $exe = Join-Path $Root 'target\release\limbochat-engine.exe'
    $count = & $exe --export-keys (Join-Path $out 'klucze-limboai.json')
    switch ($LASTEXITCODE) {
        0 { Say "Dołączono kluczy: $count" 'Green' }
        2 { Say 'Nie masz zapisanych kluczy w panelu (⚙ → LimboAI) — paczka bez kluczy.' 'Yellow' }
        default { Fail 'Nie udało się odczytać kluczy z sejfu Windows.' }
    }
}
$zip = "$out.zip"
Compress-Archive -Path (Join-Path $out '*') -DestinationPath $zip -Force
Write-Host ''
Say 'Gotowe! Wyślij znajomemu ten plik:' 'Green'
Say $zip 'White'
if ($updates) { Say 'Kolejne wersje dostanie sam (wydaj-aktualizacje.bat).' 'Green' }
Write-Host ''
