# Nowa wersja: budowanie, podpis, wydanie na GitHubie. Znajomi dostaną ją sami.
. (Join-Path $PSScriptRoot 'wspolne.ps1')

if (-not (Use-UpdateConfig)) { Fail 'Najpierw uruchom ustaw-aktualizacje.bat (raz).' }
if (-not (Test-Path $KeyFile)) { Fail "Brak klucza podpisu: $KeyFile — przywróć go z kopii albo uruchom ustaw-aktualizacje.bat." }
$repo = $env:LIMBOCHAT_UPDATE_REPO

$current = Get-EngineVersion
Write-Host ''
Say 'WYDANIE AKTUALIZACJI LiMBOCHAT' 'White'
Say "Repozytorium: $repo   ·   obecna wersja: $current"
$parts = $current.Split('.')
$suggest = "$($parts[0]).$($parts[1]).$([int]$parts[2] + 1)"
$version = (Read-Host "  Nowa wersja [Enter = $suggest]").Trim()
if (-not $version) { $version = $suggest }
if ($version -notmatch '^\d+\.\d+\.\d+$') { Fail 'Wersja musi wyglądać tak: 0.6.0' }
if ([version]$version -le [version]$current) { Fail "Nowa wersja musi być wyższa niż $current." }
$notes = (Read-Host '  Co nowego (jedno zdanie, widzą to znajomi)').Trim()
if (-not $notes) { $notes = 'Poprawki i ulepszenia.' }

Set-EngineVersion $version
$script:Bumped = $true
# Każdy błąd od tej chwili przywraca poprzedni numer wersji (można od razu ponowić).
function Fail($text) {
    if ($script:Bumped) { Set-EngineVersion $current }
    Write-Host ''; Write-Host "  $text" -ForegroundColor Red; Write-Host ''
    exit 1
}
Build-All

$asset = 'limbochat-engine-windows-x64.exe'
$out = Join-Path $Root "wydania\v$version"
New-Item -ItemType Directory -Force -Path $out | Out-Null
Copy-Item (Join-Path $Root 'target\release\limbochat-engine.exe') (Join-Path $out $asset) -Force
Engine @('--sign', (Join-Path $out $asset), $version, $asset, $KeyFile, (Join-Path $out "$asset.sig")) | Out-Null
Say 'Podpisano.' 'Green'

# Paczka dla nowych znajomych (wtyczka z wbudowanym silnikiem).
$pkg = Join-Path $out 'LiMBOCHAT-OBS'
New-Item -ItemType Directory -Force -Path $pkg | Out-Null
Copy-Item (Join-Path $Root 'target\release\limbochat.dll') $pkg -Force
Copy-Item (Join-Path $Root 'obs-plugin\dla-znajomego\ZAINSTALUJ.bat') $pkg -Force
Copy-Item (Join-Path $Root 'obs-plugin\dla-znajomego\INSTRUKCJA.txt') $pkg -Force
Compress-Archive -Path (Join-Path $pkg '*') -DestinationPath (Join-Path $out 'LiMBOCHAT-OBS.zip') -Force
Remove-Item $pkg -Recurse -Force

$files = @((Join-Path $out $asset), (Join-Path $out "$asset.sig"), (Join-Path $out 'LiMBOCHAT-OBS.zip'))
if (Get-Command gh -ErrorAction SilentlyContinue) {
    Ensure-RepoNotEmpty $repo
    Say 'Wysyłanie na GitHuba...' 'Cyan'
    & gh release create "v$version" @files --repo $repo --title "LiMBOCHAT $version" --notes $notes
    if ($LASTEXITCODE -ne 0) {
        # Sprzątanie: nieudany szkic wydania i numer wersji (żeby ponowić z tym samym numerem).
        Invoke-Quiet { gh release delete "v$version" --repo $repo --yes --cleanup-tag } | Out-Null
        Fail 'GitHub odrzucił wydanie (komunikat powyżej). Wersja wróciła do poprzedniej — możesz spróbować ponownie.'
    }
    Write-Host ''
    Say '============================================================' 'Green'
    Say " Wydano LiMBOCHAT $version!" 'Green'
    Say '============================================================' 'Green'
} else {
    Start-Process "https://github.com/$repo/releases/new?tag=v$version&title=LiMBOCHAT%20$version"
    Start-Process explorer.exe $out
    Write-Host ''
    Say 'Otworzyłem stronę nowego wydania i folder z plikami.' 'Yellow'
    Say "Przeciągnij na stronę 3 pliki z folderu, wpisz opis: $notes" 'Yellow'
    Say 'i kliknij „Publish release”.' 'Yellow'
}
if (Get-Command gh -ErrorAction SilentlyContinue) {
    $answer = Read-Host '  Wysłać też kod źródłowy tej wersji na GitHuba? (t/n)'
    if ($answer -match '^[tTyY]') { Publish-Source $repo "LiMBOCHAT $version — $notes" }
}
Write-Host ''
Say 'Wtyczki u Ciebie i znajomych pobiorą nową wersję w ciągu ok. 20 minut'
Say '(albo od razu: ⚙ → Aktualizacje → Sprawdź teraz) i zainstalują ją,'
Say 'gdy LimboAI nic nie robi — bez restartu OBS.'
Write-Host ''
