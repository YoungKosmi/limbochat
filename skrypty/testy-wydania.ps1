# Przeprowadza wydanie na atrapach; nie uruchamia Cargo ani prawdziwego gh.
$ErrorActionPreference = 'Stop'
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('limbochat-release-tests-' + [guid]::NewGuid().ToString('N'))
$utf8 = New-Object Text.UTF8Encoding $true
function Put($relative, $content) {
    $path = Join-Path $fixture $relative
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $path) | Out-Null
    [IO.File]::WriteAllText($path, $content, $utf8)
}
function Assert($condition, $message) { if (-not $condition) { throw $message } }
try {
    $common = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'wspolne.ps1'))
    $stubs = @'
$KeyFile = Join-Path $Root 'fixture.key'
function Use-UpdateConfig { $env:LIMBOCHAT_UPDATE_REPO = 'test/fixture'; return $true }
function Ensure-RepoNotEmpty($repo) {}
function Build-All {
    New-Item -ItemType Directory -Force -Path (Join-Path $Root 'target/release') | Out-Null
    [IO.File]::WriteAllText((Join-Path $Root 'target/release/limbochat-engine.exe'), 'fixture')
    [IO.File]::WriteAllText((Join-Path $Root 'target/release/limbochat.dll'), 'fixture')
}
function Engine([string[]]$engineArgs) { [IO.File]::WriteAllText($engineArgs[-1], 'signature-fixture') }
function New-BrowserPackage($zipPath) { [IO.File]::WriteAllText($zipPath, 'zip-fixture') }
function Publish-Source($repo, $message) {
    Add-Content (Join-Path $Root 'calls.txt') 'source'
    return 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
}
function gh {
    Add-Content (Join-Path $Root 'calls.txt') ('gh ' + ($args -join ' '))
    $mode = [IO.File]::ReadAllText((Join-Path $Root 'mode.txt'))
    $global:LASTEXITCODE = 0
    if ($args[0] -eq 'api' -or $args[1] -eq 'view') {
        if ($mode -ne 'existing') { $global:LASTEXITCODE = 1 }
    } elseif ($args[1] -eq 'create' -and $mode -eq 'failure') { $global:LASTEXITCODE = 1 }
}
'@
    Put 'skrypty/wspolne.ps1' ($common + "`r`n" + $stubs)
    Put 'skrypty/wydaj-aktualizacje.ps1' ([IO.File]::ReadAllText((Join-Path $PSScriptRoot 'wydaj-aktualizacje.ps1')))
    Put 'fixture.key' 'NOT-A-REAL-KEY'
    Put 'obs-plugin/dla-znajomego/ZAINSTALUJ.bat' 'fixture'
    Put 'obs-plugin/dla-znajomego/INSTRUKCJA.txt' 'fixture'
    foreach ($mode in @('success', 'failure', 'existing')) {
        Put 'mode.txt' $mode
        Put 'calls.txt' ''
        Put 'obs-plugin/Cargo.toml' "[package]`r`nversion = `"0.6.6`"`r`n"
        Put 'CHANGELOG.md' "# Zmiany`r`n`r`n## Następna wersja`r`n`r`n- Poprawki — polski opis.`r`n"
        & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $fixture 'skrypty/wydaj-aktualizacje.ps1') -Auto *> (Join-Path $fixture "$mode.log")
        $code = $LASTEXITCODE
        $calls = [IO.File]::ReadAllText((Join-Path $fixture 'calls.txt'))
        $manifest = [IO.File]::ReadAllText((Join-Path $fixture 'obs-plugin/Cargo.toml'))
        Assert ($calls -notmatch 'release delete') 'Nie wolno usuwać wydania po błędzie.'
        if ($mode -eq 'existing') {
            Assert ($code -ne 0 -and $calls -notmatch 'source|release create' -and $manifest -match '0.6.6') 'Istniejący tag nie zablokował wydania.'
        } else {
            Assert ($calls.IndexOf('source') -lt $calls.IndexOf('gh release create')) 'Tag powstał przed źródłami.'
            Assert ($calls -match '--target a{40}' -and $calls -match '--notes-file') 'Brak SHA źródeł lub pliku opisu.'
            Assert ($manifest -match '0.6.7') 'Wersja powinna pozostać zgodna z opublikowanymi źródłami.'
            Assert (($mode -eq 'success' -and $code -eq 0) -or ($mode -eq 'failure' -and $code -ne 0)) "Nieprawidłowy kod zakończenia: $mode ($code)."
        }
    }
    Write-Host 'OK: wydanie, błędy sieci, istniejące tagi, zgodność tagu ze źródłami.' -ForegroundColor Green
} finally {
    $resolved = [IO.Path]::GetFullPath($fixture)
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if ($resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and (Split-Path -Leaf $resolved) -like 'limbochat-release-tests-*') {
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
