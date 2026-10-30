# LiMBOCHAT na komputer (bez OBS): ZIP z silnikiem i LiMBOCHAT.bat — panel jako szklane okno.
. (Join-Path $PSScriptRoot 'wspolne.ps1')

$updates = Use-UpdateConfig
if (-not $updates) { Say 'UWAGA: autoaktualizacje nie są ustawione (ustaw-aktualizacje.bat).' 'Yellow' }
Say '[budowanie] silnik LiMBOCHAT...' 'Cyan'
Invoke-Cargo @('build', '--release', '-p', 'limbochat-obs', '--bin', 'limbochat-engine')

Write-Host ''
Say 'Dołączyć Twoje klucze LimboAI? (tylko dla zaufanych — używają Twoich limitów)' 'White'
$withKeys = (Read-Host '  Dołączyć? (t/n)') -match '^[tTyY]'
$zip = Join-Path $Root 'LiMBOCHAT-na-komputer.zip'
New-BrowserPackage $zip -WithKeys:$withKeys
Write-Host ''
Say 'Gotowe! Paczka:' 'Green'
Say $zip 'White'
Say 'Rozpakuj i kliknij LiMBOCHAT.bat — panel otworzy się jako osobne okno.'
Write-Host ''
