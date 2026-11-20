$ErrorActionPreference = 'Stop'
$repo = 'composable-delivery/sf-source-format'
$zipName = "sf-source-format-windows-x86_64.zip"
$installDir = "$HOME\.local\bin"

Write-Host "Installing sf-source-format for Windows ($zipName)..."
$url = "https://github.com/$repo/releases/latest/download/$zipName"
$tmp = [System.IO.Path]::GetTempFileName() + ".zip"

Invoke-WebRequest -Uri $url -OutFile $tmp
if (-not (Test-Path $installDir)) {
    New-Item -ItemType Directory -Path $installDir | Out-Null
}

$extractDir = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), [System.Guid]::NewGuid().ToString())
Expand-Archive -Path $tmp -DestinationPath $extractDir -Force
Move-Item -Path "$extractDir\sf-source-format.exe" -Destination "$installDir\sf-source-format.exe" -Force
Remove-Item $tmp -Force
Remove-Item $extractDir -Recurse -Force

Write-Host "Successfully installed sf-source-format to $installDir\sf-source-format.exe"
Write-Host "Verify installation by running: sf-source-format --help"
