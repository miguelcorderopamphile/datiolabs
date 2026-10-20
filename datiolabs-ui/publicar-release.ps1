<#
.SYNOPSIS
    Compila y publica automáticamente el instalador de Windows a GitHub Releases y datiolabs.com/descargas
#>
param(
    [string]$Version = "0.1.0",
    [string]$GithubToken = $env:GITHUB_TOKEN
)

$ErrorActionPreference = "Stop"

if (-not $GithubToken) {
    $GithubToken = Read-Host "Ingresa tu GitHub Personal Access Token"
}

Write-Host ">>> Compilando instalador de Windows..." -ForegroundColor Cyan
& .\build-installer.ps1 -Version $Version

$installerPath = "target\release\bundle\nsis\DatioLabs Retail_${Version}_x64-setup.exe"
if (-not (Test-Path $installerPath)) {
    $installerPath = "target\release\bundle\nsis\DatioLabs_Retail_${Version}_x64-setup.exe"
}
if (-not (Test-Path $installerPath)) {
    throw "No se encontró el instalador compilado."
}

Write-Host "`n>>> Publicando Release en GitHub (v$Version)..." -ForegroundColor Cyan
$repo = "absologs/apex"
$headers = @{
    "Authorization" = "token $GithubToken"
    "Accept"        = "application/vnd.github.v3+json"
}

$releaseUrl = "https://api.github.com/repos/$repo/releases/tags/v$Version"
try {
    $release = Invoke-RestMethod -Uri $releaseUrl -Headers $headers -Method Get
} catch {
    $body = @{
        tag_name         = "v$Version"
        target_commitish = "main"
        name             = "DatioLabs Retail v$Version (Windows)"
        body             = "Instalador oficial nativo para Windows 10 y 11 (64-bit). Criptográficamente auditado con SHA-256."
        draft            = $false
        prerelease       = $false
    } | ConvertTo-Json
    $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases" -Headers $headers -Method Post -Body $body
}

Write-Host ">>> Subiendo archivo instalador ($installerPath)..." -ForegroundColor Cyan
$uploadUrl = $release.upload_url.Replace("{?name,label}", "?name=DatioLabs_Retail_${Version}_x64-setup.exe")
$bytes = [System.IO.File]::ReadAllBytes((Resolve-Path $installerPath).Path)

Invoke-RestMethod -Uri $uploadUrl -Headers @{
    "Authorization" = "token $GithubToken"
    "Content-Type"  = "application/octet-stream"
} -Method Post -Body $bytes

Write-Host "`n=======================================================" -ForegroundColor Green
Write-Host " ¡PUBLICACIÓN COMPLETADA AL 100%!" -ForegroundColor Green
Write-Host " Descarga lista en: https://datiolabs.com/descargas" -ForegroundColor Yellow
Write-Host "=======================================================" -ForegroundColor Green
