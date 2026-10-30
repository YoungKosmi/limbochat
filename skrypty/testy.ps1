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

    # Zgodność folderu z GitHubem: prawdziwy git na lokalnych repozytoriach (bez sieci).
    if (Get-Command git -ErrorAction SilentlyContinue) {
        function Fail($text) { throw $text }
        function Say($text, $color) {}
        function G {
            $ErrorActionPreference = 'Continue'
            & git -c user.name=test -c user.email=test@example.com -c init.defaultBranch=main -c core.autocrlf=false @args *> $null
            if ($LASTEXITCODE -ne 0) { throw "git $args" }
        }
        function Head($dir) { (& git -C $dir rev-parse HEAD).Trim() }
        function Write-Fixture($dir, $name, $text) { [IO.File]::WriteAllText((Join-Path $dir $name), $text) }
        $remote = Join-Path $fixture 'git-remote.git'
        $work = Join-Path $fixture 'git-work'
        $other = Join-Path $fixture 'git-other'
        G init -q --bare $remote
        G clone -q $remote $work
        Write-Fixture $work 'a.txt' "1`n"
        G -C $work add a.txt
        G -C $work commit -q -m one
        G -C $work push -q origin HEAD:main
        # Ktoś inny (scalony Pull Request) dodaje zmianę na GitHubie.
        G clone -q $remote $other
        Write-Fixture $other 'b.txt' "2`n"
        G -C $other add b.txt
        G -C $other commit -q -m two
        G -C $other push -q origin HEAD:main
        $Root = $work
        $script:GitState = $null
        $failed = $false
        try { Assert-SourceIncludes (Head $other) } catch { $failed = $true }
        Assert $failed 'Wysłanie kodu bez zmian z GitHuba powinno się zatrzymać.'
        Sync-BeforeRelease
        Assert (Test-Path (Join-Path $work 'b.txt')) 'Przed wydaniem nie pobrano zmian z GitHuba.'
        Assert-SourceIncludes (Head $other)
        Write-Fixture $work 'c.txt' "3`n"
        $failed = $false
        try { Sync-BeforeRelease } catch { $failed = $true }
        Assert $failed 'Wydanie z niezapisanymi plikami powinno się zatrzymać.'
        Remove-Item (Join-Path $work 'c.txt')
        # Wydanie: zmieniony folder trafia na GitHuba commitem utworzonym poza folderem (jak przez API).
        Write-Fixture $work 'a.txt' "1.1`n"
        G -C $other pull -q origin main
        Write-Fixture $other 'a.txt' "1.1`n"
        G -C $other commit -q -am release
        G -C $other push -q origin HEAD:main
        Sync-AfterPublish (Head $other)
        Assert ((Head $work) -eq (Head $other)) 'Po wydaniu folder nie wskazuje opublikowanego commita.'
        Assert (-not (& git -C $work status --porcelain)) 'Po wydaniu folder powinien być czysty.'
        Assert ([IO.File]::ReadAllText((Join-Path $work 'a.txt')) -eq "1.1`n") 'Synchronizacja nie może zmieniać plików.'
        Write-Host 'OK: pobieranie zmian przed wydaniem, ochrona przed nadpisaniem, czysty folder po wydaniu.' -ForegroundColor Green
    }

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
