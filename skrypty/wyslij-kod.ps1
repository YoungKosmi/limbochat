# Wysyła kod źródłowy projektu na GitHuba (repozytorium z aktualizacji) i ustawia ładny opis.
. (Join-Path $PSScriptRoot 'wspolne.ps1')

if (-not (Test-Path $RepoFile)) { Fail 'Najpierw uruchom ustaw-aktualizacje.bat (raz).' }
$repo = ([IO.File]::ReadAllText($RepoFile)).Trim()
$version = Get-EngineVersion

Write-Host ''
Say 'KOD ŹRÓDŁOWY LiMBOCHAT → GitHub' 'White'
Say "Repozytorium: https://github.com/$repo   ·   wersja: $version"
Say 'Pliki lokalne (target, wydania, paczki, klucz podpisu) NIE są wysyłane.' 'Gray'
Write-Host ''
$message = (Read-Host "  Opis zmian [Enter = LiMBOCHAT $version]").Trim()
if (-not $message) { $message = "LiMBOCHAT $version" }
$published = Publish-Source $repo $message
Sync-AfterPublish $published
Write-Host ''
Start-Process "https://github.com/$repo"
