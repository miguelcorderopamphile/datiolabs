<# 
.SYNOPSIS
    Build DatioLabs Retail installer for Windows

.DESCRIPTION
    Compiles the Rust backend, builds the frontend, and creates the NSIS installer.
    Run from PowerShell as Administrator on Windows.

.REQUIREMENTS
    - Rust stable (msvc toolchain)
    - Node.js 20+
    - NSIS 3.0+
    - Visual Studio Build Tools / C++ Build Tools
#>

param(
    [switch]$Clean,
    [switch]$SkipFrontend,
    [string]$Version = "0.1.0"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "  DatioLabs Retail - Build Installer" -ForegroundColor Cyan
Write-Host "  Version: $Version" -ForegroundColor Cyan
Write-Host "========================================`n" -ForegroundColor Cyan

# Check prerequisites
function Check-Prerequisites {
    Write-Host "Verificando prerequisitos..." -ForegroundColor Yellow
    
    $rust = Get-Command rustc -ErrorAction SilentlyContinue
    if (-not $rust) { throw "Rust no encontrado. Instale desde https://rustup.rs/" }
    Write-Host "  ✓ Rust: $($rust.Source)" -ForegroundColor Green
    
    $cargo = Get-Command cargo -ErrorAction SilentlyContinue
    if (-not $cargo) { throw "Cargo no encontrado" }
    Write-Host "  ✓ Cargo: $($cargo.Source)" -ForegroundColor Green
    
    if (Test-Path "..\ui\dist\index.html") {
        Write-Host "  ✓ Frontend: Precompilado en ui/dist (No requiere Node ni Vite)" -ForegroundColor Green
        $script:SkipFrontend = $true
    } else {
        $node = Get-Command node -ErrorAction SilentlyContinue
        if (-not $node) { throw "Node.js no encontrado. Instale v20+ desde https://nodejs.org/" }
        Write-Host "  ✓ Node: $($node.Source) - $((node --version))" -ForegroundColor Green
        
        $npm = Get-Command npm -ErrorAction SilentlyContinue
        if (-not $npm) { throw "npm no encontrado" }
        Write-Host "  ✓ npm: $($npm.Source)" -ForegroundColor Green
    }
    
    $makensis = Get-Command makensis -ErrorAction SilentlyContinue
    if (-not $makensis) { 
        Write-Host "  ⚠ NSIS (makensis) no en PATH. Se usará el de Tauri si está disponible." -ForegroundColor Yellow
    } else {
        Write-Host "  ✓ NSIS: $($makensis.Source)" -ForegroundColor Green
    }
    
    Write-Host ""
}

# Generate icons if needed
function Generate-Icons {
    Write-Host "Generando iconos..." -ForegroundColor Yellow
    
    $iconPath = "icons/icon.png"
    if (-not (Test-Path $iconPath)) { throw "icon.png no encontrado en $iconPath" }
    
    if (-not (Get-Command magick -ErrorAction SilentlyContinue)) {
        Write-Host "  ⚠ ImageMagick (magick) no encontrado. Use iconos pregenerados o instale ImageMagick." -ForegroundColor Yellow
        return
    }
    
    magick $iconPath -define icon:auto-resize=256,128,64,48,32,16 icons/icon.ico
    Write-Host "  ✓ icon.ico generado" -ForegroundColor Green
    
    magick $iconPath -resize 150x57! icons/header.bmp
    Write-Host "  ✓ header.bmp generado" -ForegroundColor Green
    
    magick $iconPath -resize 498x312! icons/welcome.bmp
    Write-Host "  ✓ welcome.bmp generado" -ForegroundColor Green
    
    Write-Host ""
}

# Build frontend
function Build-Frontend {
    if ($SkipFrontend) {
        Write-Host "Saltando build del frontend (--SkipFrontend)" -ForegroundColor Yellow
        return
    }
    
    Write-Host "Building frontend..." -ForegroundColor Yellow
    Push-Location "../ui"
    
    if (-not (Test-Path "node_modules")) {
        Write-Host "  Instalando dependencias..." -ForegroundColor Cyan
        npm ci
    }
    
    Write-Host "  Compilando TypeScript + Vite..." -ForegroundColor Cyan
    npm run build
    
    if (-not (Test-Path "dist")) { throw "Build del frontend falló - no hay carpeta dist" }
    Write-Host "  ✓ Frontend build completado" -ForegroundColor Green
    
    Pop-Location
    Write-Host ""
}

# Build Rust backend + Tauri
function Build-Tauri {
    Write-Host "Building Tauri app..." -ForegroundColor Yellow
    
    $env:TAURI_VERSION = $Version
    
    if ($Clean) {
        Write-Host "  Limpiando builds anteriores..." -ForegroundColor Cyan
        cargo clean
    }
    
    Write-Host "  Compilando release (esto puede tardar unos minutos)..." -ForegroundColor Cyan
    cargo tauri build --target x86_64-pc-windows-msvc
    
    $installerPath = "target/release/bundle/nsis/DatioLabs Retail_${Version}_x64-setup.exe"
    if (-not (Test-Path $installerPath)) {
        $altPath = "target/release/bundle/nsis/DatioLabs_Retail_${Version}_x64-setup.exe"
        if (Test-Path $altPath) { $installerPath = $altPath }
        else { throw "Instalador no generado en $installerPath" }
    }
    
    Write-Host "  ✓ Instalador creado: $installerPath" -ForegroundColor Green
    Write-Host ""
    
    return $installerPath
}

# Verify installer
function Verify-Installer {
    param($Path)
    
    Write-Host "Verificando instalador..." -ForegroundColor Yellow
    
    $size = (Get-Item $Path).Length / 1MB
    Write-Host "  Tamaño: $([math]::Round($size, 2)) MB" -ForegroundColor Cyan
    
    # Quick test install in temp
    $testDir = "$env:TEMP\DatioLabsTest_$(Get-Random)"
    Write-Host "  Instalación de prueba en $testDir..." -ForegroundColor Cyan
    
    $proc = Start-Process -FilePath $Path -ArgumentList "/S", "/D=$testDir" -Wait -PassThru
    if ($proc.ExitCode -ne 0) { throw "Instalación de prueba falló (exit code: $($proc.ExitCode))" }
    
    $exePath = "$testDir\DatioLabs.exe"
    if (-not (Test-Path $exePath)) { throw "Ejecutable no encontrado tras instalación" }
    
    Write-Host "  ✓ Instalación de prueba OK" -ForegroundColor Green
    
    # Cleanup
    $uninst = "$testDir\uninstall.exe"
    if (Test-Path $uninst) {
        Start-Process -FilePath $uninst -ArgumentList "/S" -Wait
    }
    Remove-Item $testDir -Recurse -Force -ErrorAction SilentlyContinue
    
    Write-Host ""
}

# Main
try {
    Check-Prerequisites
    Generate-Icons
    Build-Frontend
    $installer = Build-Tauri
    Verify-Installer $installer
    
    Write-Host "========================================" -ForegroundColor Cyan
    Write-Host "  ✓ BUILD COMPLETADO EXITOSAMENTE" -ForegroundColor Green
    Write-Host "  Instalador: $installer" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan
}
catch {
    Write-Host "`n========================================" -ForegroundColor Red
    Write-Host "  ✗ ERROR: $_" -ForegroundColor Red
    Write-Host "========================================" -ForegroundColor Red
    exit 1
}