# Testy bez połączenia z GitHubem, Twitch i OBS. Działa też w PowerShell 5.1.
$ErrorActionPreference = 'Stop'
$project = Split-Path -Parent $PSScriptRoot
$previousLocation = Get-Location
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('limbochat-tests-' + [guid]::NewGuid().ToString('N'))
function Assert($condition, $message) { if (-not $condition) { throw $message } }
function Put($relative, $content) {
    $path = Join-Path $fixture $relative
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
    [IO.File]::WriteAllText($path, $content, (New-Object Text.UTF8Encoding $false))
}
try {
    New-Item -ItemType Directory -Path $fixture | Out-Null
    . (Join-Path $PSScriptRoot 'wspolne.ps1')
    $Root = $fixture
    Put 'obs-plugin/Cargo.toml' "[package]`nversion = `"0.6.6`"`n"
    Put 'src/main.rs' "fn main() {}`r`n"
    Put '.github/workflows/ci.yml' 'name: CI'
    foreach ($path in @('.env', '.env.local', 'private.pem', 'private.key', 'private.pfx', 'private.p12', 'import-keys.json', 'klucze-limboai.json', 'handover.json', 'aktualizacje-repo.txt', 'app.exe', 'app.dll', 'copy.bak', 'target/test.rs', 'wydania/test.rs', 'work/test.rs', 'outputs/test.rs', '.git/config', 'node_modules/test.js')) {
        Put $path 'fixture'
    }
    $paths = @(Get-SourceFiles | ForEach-Object { $_.Path })
    Assert ($paths.Count -eq 3) "Do źródeł trafiły prywatne lub wynikowe pliki: $($paths -join ', ')"
    Assert ($paths -contains '.github/workflows/ci.yml') 'Brakuje konfiguracji GitHuba w źródłach.'
    Assert ((Get-EngineVersion) -eq '0.6.6') 'Nie odczytano wersji.'
    Set-EngineVersion '0.6.7'
    Assert ((Get-EngineVersion) -eq '0.6.7') 'Nie zapisano wersji.'

    # Rzeczywista funkcja Publish-Source, ale wszystkie wywołania GitHuba są atrapami.
    function Ensure-RepoNotEmpty($repo) {}
    function gh {
        $global:LASTEXITCODE = 0
        $command = $args -join ' '
        if ($command -match '\.default_branch') { 'main' }
        elseif ($command -match 'git/ref/heads') { 'head-sha' }
        elseif ($command -match 'git/commits') { 'base-tree' }
        else { throw "Nieoczekiwane wywołanie gh: $command" }
    }
    $script:TreeResult = 'new-tree'
    $script:RefUpdated = $false
    function Invoke-GhJson($method, $endpoint, $body, $jq) {
        if ($endpoint -match '/git/blobs$') {
            $blob = $body | ConvertFrom-Json
            $text = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($blob.content))
            Assert (-not $text.Contains("`r`n")) 'Źródła w Git powinny mieć końce linii LF.'
            return 'blob-sha'
        }
        if ($endpoint -match '/git/trees$') { return $script:TreeResult }
        if ($endpoint -match '/git/commits$') {
            Assert ($body.parents[0] -eq 'head-sha') 'Commit nie zachowuje historii.'
            return 'published-sha'
        }
        if ($endpoint -match '/git/refs/heads/') {
            Assert (-not $body.force) 'Publikacja nie może wymuszać nadpisania historii.'
            $script:RefUpdated = $true
            return 'published-sha'
        }
        return 'ok'
    }
    $sha = Publish-Source 'test/fixture' 'test'
    Assert ($sha -eq 'published-sha' -and $script:RefUpdated) 'Wydanie nie otrzyma SHA nowego commita.'
    $script:TreeResult = 'base-tree'
    $script:RefUpdated = $false
    $sha = Publish-Source 'test/fixture' 'test'
    Assert ($sha -eq 'head-sha' -and -not $script:RefUpdated) 'Niezmienione źródła powinny zwracać bieżący commit.'

    # Sprawdź składnię wszystkich skryptów także parserem hosta Windows PowerShell.
    foreach ($file in Get-ChildItem (Join-Path $project 'skrypty') -Filter '*.ps1' -File) {
        $parseErrors = $null
        $tokens = $null
        [Management.Automation.Language.Parser]::ParseFile($file.FullName, [ref]$tokens, [ref]$parseErrors) | Out-Null
        Assert ($parseErrors.Count -eq 0) "Błąd składni $($file.Name): $parseErrors"
    }
    Write-Host 'OK: filtrowanie źródeł, wersjonowanie, commit publikacji, składnia PowerShell.' -ForegroundColor Green
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $project 'skrypty/testy-wydania.ps1')
    Assert ($LASTEXITCODE -eq 0) 'Testy wydawania nie przeszły.'
} finally {
    Set-Location $previousLocation
    # Ścieżka utworzona na potrzeby testu; nigdy katalog projektu użytkownika.
    $resolved = [IO.Path]::GetFullPath($fixture)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if ($resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and (Split-Path -Leaf $resolved) -like 'limbochat-tests-*') {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
