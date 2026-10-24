# Jednorazowo: repozytorium GitHub na aktualizacje + klucz do podpisywania.
. (Join-Path $PSScriptRoot 'wspolne.ps1')

Write-Host ''
Say 'AUTOAKTUALIZACJE LiMBOCHAT — ustawienie (raz)' 'White'
Say 'Nowe wersje będziesz wrzucać na GitHuba, a wtyczki u Ciebie i znajomych'
Say 'zaktualizują się same, w trakcie streama, bez restartu OBS.'
Write-Host ''
Say 'Potrzebne: konto GitHub i PUBLICZNE repozytorium (może być puste,'
Say 'np. „limbochat” — wystarczą w nim same wydania, bez kodu).'
Write-Host ''
$repo = (Read-Host '  Repozytorium (nick/nazwa, np. kosmi/limbochat)').Trim()
if ($repo -notmatch '^[A-Za-z0-9-]+/[A-Za-z0-9._-]+$') { Fail 'To nie wygląda jak nick/nazwa repozytorium.' }

Build-All
New-Item -ItemType Directory -Force -Path $KeyDir | Out-Null
if (Test-Path $KeyFile) {
    Say "Używam istniejącego klucza: $KeyFile" 'Yellow'
    $pub = Engine @('--pubkey', $KeyFile)
} else {
    $pub = Engine @('--keygen', $KeyFile)
    Say "Nowy klucz podpisu: $KeyFile" 'Green'
}
[IO.File]::WriteAllText($RepoFile, $repo, $Utf8)
[IO.File]::WriteAllText($PubFile, ($pub | Out-String).Trim(), $Utf8)

# GitHub CLI: utwórz repozytorium, jeśli go nie ma.
if (Get-Command gh -ErrorAction SilentlyContinue) {
    if ((Invoke-Quiet { gh repo view $repo }) -ne 0) {
        $answer = Read-Host "  Repozytorium $repo nie istnieje. Utworzyć je teraz jako publiczne? (t/n)"
        if ($answer -match '^[tTyY]') {
            & gh repo create $repo --public --add-readme --description 'Wydania LiMBOCHAT (autoaktualizacje)'
            if ($LASTEXITCODE -ne 0) { Say 'Nie udało się — utwórz je ręcznie na github.com/new.' 'Yellow' }
        }
    }
} else {
    Say 'Nie masz GitHub CLI (gh) — wydania wrzucisz przez stronę albo zainstaluj:' 'Yellow'
    Say 'https://cli.github.com  →  potem w terminalu: gh auth login' 'Yellow'
}

Write-Host ''
Say '============================================================' 'Green'
Say ' Gotowe! Autoaktualizacje ustawione.' 'Green'
Say '============================================================' 'Green'
Write-Host ''
Say 'Dalej:'
Say "1. Repozytorium: https://github.com/$repo (musi być publiczne)."
Say '2. Zamknij OBS i uruchom install-obs-plugin.bat (ta wersja umie się aktualizować).'
Say '3. Wyślij znajomym NOWĄ paczkę (pakiet-dla-znajomego.bat) — ostatni raz ręcznie.'
Say '4. Każda kolejna wersja: wydaj-aktualizacje.bat — reszta dzieje się sama.'
Write-Host ''
Say "WAŻNE: zrób kopię pliku $KeyFile (np. na pendrive)." 'Yellow'
Say 'Bez niego nie wydasz aktualizacji dla już zainstalowanych kopii.' 'Yellow'
Say 'Nigdy go nikomu nie wysyłaj i nie wrzucaj na GitHuba.' 'Yellow'
Write-Host ''
