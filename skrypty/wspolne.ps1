# Wspólne funkcje skryptów LiMBOCHAT (budowanie silnika i wtyczki).
$ErrorActionPreference = 'Stop'
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root
$KeyDir = Join-Path $env:USERPROFILE '.limbochat'
$KeyFile = Join-Path $KeyDir 'klucz-aktualizacji.key'
$RepoFile = Join-Path $Root 'aktualizacje-repo.txt'
$PubFile = Join-Path $Root 'aktualizacje-klucz-publiczny.txt'
$Utf8 = New-Object System.Text.UTF8Encoding($false)

function Say($text, $color = 'Gray') { Write-Host "  $text" -ForegroundColor $color }
function Fail($text) { Write-Host ""; Write-Host "  $text" -ForegroundColor Red; Write-Host ""; exit 1 }

function Get-EngineVersion {
    $line = Select-String -Path (Join-Path $Root 'obs-plugin\Cargo.toml') -Pattern '^version = "([^"]+)"' | Select-Object -First 1
    if (-not $line) { Fail 'Nie znalazłem wersji w obs-plugin\Cargo.toml.' }
    $line.Matches[0].Groups[1].Value
}

function Set-EngineVersion($version) {
    $path = Join-Path $Root 'obs-plugin\Cargo.toml'
    $text = [IO.File]::ReadAllText($path)
    $re = [regex]'(?m)^version = "[^"]*"'
    [IO.File]::WriteAllText($path, $re.Replace($text, "version = `"$version`"", 1), $Utf8)
}

# Repozytorium i klucz publiczny aktualizacji (jeśli ustawione) trafiają do silnika przy budowaniu.
function Use-UpdateConfig {
    if ((Test-Path $RepoFile) -and (Test-Path $PubFile)) {
        $env:LIMBOCHAT_UPDATE_REPO = ([IO.File]::ReadAllText($RepoFile)).Trim()
        $env:LIMBOCHAT_UPDATE_PUBKEY = ([IO.File]::ReadAllText($PubFile)).Trim()
        return $true
    }
    Remove-Item Env:LIMBOCHAT_UPDATE_REPO -ErrorAction SilentlyContinue
    Remove-Item Env:LIMBOCHAT_UPDATE_PUBKEY -ErrorAction SilentlyContinue
    return $false
}

# Program, którego błędy są spodziewane (np. „repozytorium puste”): bez przerywania skryptu,
# wyjście schowane, zwraca kod zakończenia. (Windows PowerShell 5.1 przy ErrorAction=Stop
# traktuje każdy tekst na stderr jako błąd krytyczny.)
function Invoke-Quiet([scriptblock]$command) {
    $old = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try { & $command *> $null } catch { }
    $code = $LASTEXITCODE
    $ErrorActionPreference = $old
    return $code
}

function Invoke-Cargo([string[]]$cargoArgs) {
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { Fail 'Budowanie nie powiodło się — sprawdź komunikat powyżej.' }
}

# Buduje silnik, a potem wtyczkę OBS z wbudowanym silnikiem.
function Build-All {
    Say '[budowanie] silnik LiMBOCHAT...' 'Cyan'
    Invoke-Cargo @('build', '--release', '-p', 'limbochat-obs', '--bin', 'limbochat-engine')
    $env:LIMBOCHAT_ENGINE_EXE = Join-Path $Root 'target\release\limbochat-engine.exe'
    Say '[budowanie] wtyczka OBS...' 'Cyan'
    Invoke-Cargo @('build', '--release', '-p', 'limbochat-obs-launcher')
    Remove-Item Env:LIMBOCHAT_ENGINE_EXE -ErrorAction SilentlyContinue
}

function Engine([string[]]$engineArgs) {
    $exe = Join-Path $Root 'target\release\limbochat-engine.exe'
    $out = & $exe @engineArgs
    if ($LASTEXITCODE -ne 0) { Fail "Silnik zwrócił błąd ($($engineArgs[0]))." }
    $out
}

# ── GitHub: kod źródłowy (przez GitHub CLI, bez instalowania Gita) ─────────

# Wydanie i kod potrzebują choć jednego commita w repozytorium (np. README).
function Ensure-RepoNotEmpty($repo) {
    if ((Invoke-Quiet { gh api "repos/$repo/commits?per_page=1" }) -eq 0) { return }
    Say 'Repozytorium jest puste — dodaję README (GitHub wymaga choć jednego commita)...' 'Yellow'
    $readme = "# LiMBOCHAT`n`nCzat Twitcha w OBS.`n"
    $b64 = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($readme))
    if ((Invoke-Quiet { gh api -X PUT "repos/$repo/contents/README.md" -f message='Start' -f content=$b64 }) -ne 0) {
        Fail "Nie udało się dodać README. Wejdź na https://github.com/$repo, kliknij „Add a README” → Commit i uruchom ponownie."
    }
}

# Pliki projektu bez rzeczy lokalnych i prywatnych (kompilacja, wydania, paczki, wszystkie klucze).
function Get-SourceFiles {
    $skipDirs = @('target', 'wydania', 'LiMBOCHAT-OBS-dla-znajomego', '.git', 'node_modules', 'work', 'outputs', '.idea', '.vscode')
    $files = New-Object System.Collections.Generic.List[object]
    $walk = {
        param($dir, $prefix)
        foreach ($item in Get-ChildItem -LiteralPath $dir -Force) {
            $rel = if ($prefix) { "$prefix/$($item.Name)" } else { $item.Name }
            if ($item.PSIsContainer) {
                if ($skipDirs -notcontains $item.Name) { & $walk $item.FullName $rel }
            } elseif ($item.Name -notmatch '\.(key|pem|pfx|p12|zip|log|exe|dll|tmp|bak)$' -and $item.Name -notmatch '^\.env($|\.)|^(aktualizacje-.*\.txt|klucze-limboai\.json|import-keys\.json|handover\.json)$') {
                $files.Add([pscustomobject]@{ Path = $rel; Full = $item.FullName })
            }
        }
    }
    & $walk $Root ''
    $files
}

# Wywołanie API z treścią JSON z pliku (długie treści nie mieszczą się w linii poleceń).
function Invoke-GhJson($method, $endpoint, $body, $jq) {
    $tmp = [IO.Path]::GetTempFileName()
    try {
        # Gotowy tekst JSON przechodzi bez zmian (duże pliki — ConvertTo-Json w PS 5.1 ma limit 2 MB).
        $json = if ($body -is [string]) { $body } else { $body | ConvertTo-Json -Depth 8 -Compress }
        [IO.File]::WriteAllText($tmp, $json, $Utf8)
        $out = & gh api -X $method $endpoint --input $tmp --jq $jq
        if ($LASTEXITCODE -ne 0) { Fail "GitHub odrzucił zapytanie ($endpoint)." }
        "$out".Trim()
    } finally {
        Remove-Item $tmp -ErrorAction SilentlyContinue
    }
}

# Wysyła cały kod jako jeden commit na główną gałąź + ładny opis repozytorium.
function Publish-Source($repo, $message) {
    if (-not (Get-Command gh -ErrorAction SilentlyContinue)) {
        Fail 'Potrzebny GitHub CLI (https://cli.github.com) i logowanie: gh auth login'
    }
    Ensure-RepoNotEmpty $repo
    $branch = (& gh api "repos/$repo" --jq .default_branch).Trim()
    $head = (& gh api "repos/$repo/git/ref/heads/$branch" --jq .object.sha).Trim()
    $baseTree = (& gh api "repos/$repo/git/commits/$head" --jq .tree.sha).Trim()

    $files = @(Get-SourceFiles)
    Say "Wysyłanie kodu: $($files.Count) plików..." 'Cyan'
    $tree = New-Object System.Collections.Generic.List[object]
    $i = 0
    foreach ($f in $files) {
        $i++
        Write-Progress -Activity 'Wysyłanie kodu na GitHuba' -Status $f.Path -PercentComplete ([int](100 * $i / $files.Count))
        $bytes = [IO.File]::ReadAllBytes($f.Full)
        # Tak samo jak Git + .gitattributes: tekst w repozytorium ma LF.
        # UTF-8 BOM skryptów PowerShell pozostaje zachowany dla Windows PowerShell 5.1.
        if ($f.Path -match '\.(rs|toml|lock|md|js|css|html|yml|yaml|ps1|bat|txt)$|(^|/)(\.gitignore|\.gitattributes|\.editorconfig)$') {
            $bytes = [Text.Encoding]::UTF8.GetBytes([Text.Encoding]::UTF8.GetString($bytes).Replace("`r`n", "`n"))
        }
        $content = [Convert]::ToBase64String($bytes)
        $sha = Invoke-GhJson 'POST' "repos/$repo/git/blobs" ('{"encoding":"base64","content":"' + $content + '"}') '.sha'
        $tree.Add(@{ path = $f.Path; mode = '100644'; type = 'blob'; sha = $sha })
    }
    Write-Progress -Activity 'Wysyłanie kodu na GitHuba' -Completed

    # Bez base_tree: repozytorium ma dokładnie te pliki (usunięte lokalnie znikają też tam).
    $treeSha = Invoke-GhJson 'POST' "repos/$repo/git/trees" @{ tree = $tree.ToArray() } '.sha'
    $publishedHead = $head
    if ($treeSha -eq $baseTree) {
        Say 'Kod na GitHubie jest już aktualny — nic do wysłania.' 'Green'
    } else {
        $commit = Invoke-GhJson 'POST' "repos/$repo/git/commits" @{ message = $message; tree = $treeSha; parents = @($head) } '.sha'
        Invoke-GhJson 'PATCH' "repos/$repo/git/refs/heads/$branch" @{ sha = $commit; force = $false } '.object.sha' | Out-Null
        $publishedHead = $commit
        Say "Kod wysłany: https://github.com/$repo" 'Green'
    }

    # Opis, strona i tematy repozytorium.
    $about = @{
        description = 'Czat Twitcha w OBS: emotki 7TV, moderacja, asystent głosowy LimboAI, animowane alerty, overlay i autoaktualizacje. Rust.'
        homepage = "https://github.com/$repo/releases/latest"
        has_wiki = $false
    }
    Invoke-GhJson 'PATCH' "repos/$repo" $about '.full_name' | Out-Null
    $topics = @{ names = @('twitch', 'twitch-chat', 'obs', 'obs-plugin', 'obs-studio', '7tv', 'rust', 'streaming', 'moderation', 'voice-assistant', 'gemini', 'overlay', 'polish') }
    Invoke-GhJson 'PUT' "repos/$repo/topics" $topics '.names | length' | Out-Null
    Say 'Opis i tematy repozytorium ustawione.' 'Green'
    return $publishedHead
}

# Paczka „na komputer” (bez OBS): silnik + LiMBOCHAT.bat, który otwiera panel jako okno.
function New-BrowserPackage($zipPath, [switch]$WithKeys) {
    $dir = Join-Path ([IO.Path]::GetTempPath()) "LiMBOCHAT-przegladarka-$PID"
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    New-Item -ItemType Directory -Path $dir | Out-Null
    Copy-Item (Join-Path $Root 'target\release\limbochat-engine.exe') $dir
    Copy-Item (Join-Path $Root 'obs-plugin\przegladarka\*') $dir
    if ($WithKeys) {
        & (Join-Path $Root 'target\release\limbochat-engine.exe') --export-keys (Join-Path $dir 'klucze-limboai.json') | Out-Null
    }
    Compress-Archive -Path (Join-Path $dir '*') -DestinationPath $zipPath -Force
    Remove-Item $dir -Recurse -Force
}
