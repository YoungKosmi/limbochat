# Nowa wersja: budowanie, podpis, wydanie na GitHubie. Znajomi dostaną ją sami.
# -Auto: bez pytań (następny numer wersji, kod źródłowy też na GitHuba), log w wydania\ostatnie-wydanie.log.
param([switch]$Auto)
. (Join-Path $PSScriptRoot 'wspolne.ps1')
if ($Auto) {
    New-Item -ItemType Directory -Force -Path (Join-Path $Root 'wydania') | Out-Null
    Start-Transcript -Path (Join-Path $Root 'wydania\ostatnie-wydanie.log') -Force | Out-Null
    function Read-Host($prompt) { Write-Host "$prompt -> (automatycznie)"; return $(if ($prompt -match 'kod') { 't' } else { '' }) }
}

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
# Lista zmian: punkty z sekcji „## Następna wersja” w CHANGELOG.md trafiają do okienka
# „Co nowego” po aktualizacji i do opisu wydania na GitHubie.
$changelogPath = Join-Path $Root 'CHANGELOG.md'
$changelogOld = if (Test-Path $changelogPath) { [IO.File]::ReadAllText($changelogPath) } else { "# Co nowego w LiMBOCHAT`n" }
$lines = $changelogOld -split "`r?`n"
$start = [Array]::FindIndex($lines, [Predicate[string]] { param($l) $l.Trim() -eq '## Następna wersja' })
$items = @()
if ($start -ge 0) {
    for ($i = $start + 1; $i -lt $lines.Count -and -not $lines[$i].StartsWith('## '); $i++) {
        if ($lines[$i] -match '^\s*- (.+)$') { $items += $Matches[1].Trim() }
    }
}
if ($items.Count) {
    Say "Co nowego (z CHANGELOG.md, $($items.Count) pkt):"
    foreach ($it in $items) { Write-Host "   • $($it -replace '\*\*', '')" }
} else {
    $one = (Read-Host '  Co nowego (jedno zdanie, widzą to znajomi)').Trim()
    if (-not $one) { $one = 'Poprawki i ulepszenia.' }
    $items = @($one)
}
$notes = ($items | ForEach-Object { "- $_" }) -join "`n"
$date = Get-Date -Format 'yyyy-MM-dd'
$section = @("## Następna wersja", '', "## $version — $date", '') + @($items | ForEach-Object { "- $_" })
if ($start -ge 0) {
    $end = $start + 1
    while ($end -lt $lines.Count -and -not $lines[$end].StartsWith('## ')) { $end++ }
    $before = @(if ($start -gt 0) { $lines[0..($start - 1)] })
    $after = @(if ($end -lt $lines.Count) { ''; $lines[$end..($lines.Count - 1)] })
    $lines = @($before) + @($section) + @($after)
} else {
    $head = [Array]::FindIndex($lines, [Predicate[string]] { param($l) $l.StartsWith('## ') })
    if ($head -lt 0) { $lines = $lines + @('') + $section }
    else { $lines = @(if ($head -gt 0) { $lines[0..($head - 1)] }) + @($section) + @('') + @($lines[$head..($lines.Count - 1)]) }
}
[IO.File]::WriteAllText($changelogPath, (($lines -join "`r`n").TrimEnd() + "`r`n"), (New-Object Text.UTF8Encoding $false))

Set-EngineVersion $version
$script:Bumped = $true
# Każdy błąd od tej chwili przywraca poprzedni numer wersji i CHANGELOG (można od razu ponowić).
function Fail($text) {
    if ($script:Bumped) {
        Set-EngineVersion $current
        [IO.File]::WriteAllText($changelogPath, $changelogOld, (New-Object Text.UTF8Encoding $false))
    }
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

# Wersja na komputer bez OBS (panel jako okno) — bez kluczy, dla wszystkich.
New-BrowserPackage (Join-Path $out 'LiMBOCHAT-na-komputer.zip')

$files = @((Join-Path $out $asset), (Join-Path $out "$asset.sig"), (Join-Path $out 'LiMBOCHAT-OBS.zip'), (Join-Path $out 'LiMBOCHAT-na-komputer.zip'))
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
    Say "Przeciągnij na stronę 4 pliki z folderu, wpisz opis z CHANGELOG.md (sekcja $version)" 'Yellow'
    Say 'i kliknij „Publish release”.' 'Yellow'
}
if (Get-Command gh -ErrorAction SilentlyContinue) {
    $answer = Read-Host '  Wysłać też kod źródłowy tej wersji na GitHuba? (t/n)'
    if ($answer -match '^[tTyY]') { Publish-Source $repo "LiMBOCHAT $version — $($items[0] -replace '\*\*', '')" }
}
Write-Host ''
Say 'Wtyczki u Ciebie i znajomych pobiorą nową wersję w ciągu ok. 20 minut'
Say '(albo od razu: ⚙ → Aktualizacje → Sprawdź teraz) i zainstalują ją,'
Say 'gdy LimboAI nic nie robi — bez restartu OBS.'
Write-Host ''
