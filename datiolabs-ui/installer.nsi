; DatioLabs - Instalador Oficial
; Codificación UTF-8 para evitar caracteres rotos
Unicode true

SetCompressor /SOLID lzma
SetCompressorDictSize 32
SetDatablockOptimize on

!define PRODUCT_NAME "DatioLabs"
Name "${PRODUCT_NAME}"
!define PRODUCT_VERSION "1.0.0"
!define PRODUCT_PUBLISHER "DatioLabs"
!define PRODUCT_WEB_SITE "https://datiolabs.com"
!define PRODUCT_DIR_REGKEY "Software\Microsoft\Windows\CurrentVersion\App Paths\DatioLabs.exe"
!define PRODUCT_UNINST_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\DatioLabs"
!define PRODUCT_UNINST_ROOT_KEY "HKCU"
!define OUT_FILE "DatioLabs_${PRODUCT_VERSION}_x64-setup.exe"

OutFile "${OUT_FILE}"

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"

; Iconos corporativos 'D.'
!define MUI_ICON "icons\icon.ico"
!define MUI_UNICON "icons\icon.ico"

; Cabecera oficial
!define MUI_HEADERIMAGE
!define MUI_HEADERIMAGE_BITMAP "icons\header.bmp"
!define MUI_HEADERIMAGE_RIGHT

; Textos con personalidad e identidad de DatioLabs
!define MUI_WELCOMEPAGE_TITLE "Instalador Oficial de DatioLabs v${PRODUCT_VERSION}"
!define MUI_WELCOMEPAGE_TEXT "Bienvenido a DatioLabs.$\n$\nSistema determinista de punto de venta, inventario multirubro atómico y auditoría financiera local.$\n$\nPresione Siguiente para continuar con la instalación en este equipo."

!define MUI_FINISHPAGE_TITLE "Instalación Completada con Éxito"
!define MUI_FINISHPAGE_TEXT "DatioLabs v${PRODUCT_VERSION} está listo para operar en este terminal.$\n$\nAl iniciar, el asistente interactivo le permitirá configurar un nuevo negocio o restaurar datos desde un archivo de respaldo criptográfico."
!define MUI_FINISHPAGE_RUN "$INSTDIR\DatioLabs.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Iniciar DatioLabs ahora"

; Flujo de páginas limpio: Bienvenida -> Carpeta de Instalación -> Instalación de Archivos -> Finalizar
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

; Páginas de Desinstalación
!define MUI_UNWELCOMEPAGE_TITLE "Desinstalar DatioLabs"
!define MUI_UNWELCOMEPAGE_TEXT "Este asistente desinstalará DatioLabs de su computadora.$\n$\nDurante el proceso podrá elegir si desea conservar o purgar totalmente su base de datos local."
!insertmacro MUI_UNPAGE_WELCOME
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_UNPAGE_FINISH

!insertmacro MUI_LANGUAGE "Spanish"

; Directorio de instalación predeterminado (por usuario, no requiere elevación de UAC)
InstallDir "$LOCALAPPDATA\DatioLabs"
InstallDirRegKey HKCU "${PRODUCT_DIR_REGKEY}" ""
RequestExecutionLevel user

Section "MainSection" SEC01
  SetOutPath "$INSTDIR"
  
  ; Copiar binario y dependencias
  File /r "app\*"
  
  ; Crear desinstalador
  WriteUninstaller "$INSTDIR\uninstall.exe"
  
  ; Registro de desinstalación de Windows
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayName" "${PRODUCT_NAME}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayVersion" "${PRODUCT_VERSION}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "Publisher" "${PRODUCT_PUBLISHER}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "URLInfoAbout" "${PRODUCT_WEB_SITE}"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${PRODUCT_UNINST_KEY}" "DisplayIcon" "$INSTDIR\DatioLabs.exe"
  
  ; App Paths para ejecutar desde Windows Run (Win+R -> DatioLabs)
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\DatioLabs.exe" "" "$INSTDIR\DatioLabs.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\DatioLabs.exe" "Path" "$INSTDIR"
  
  ; Accesos directos en Menú Inicio
  CreateDirectory "$SMPROGRAMS\DatioLabs"
  CreateShortcut "$SMPROGRAMS\DatioLabs\DatioLabs.lnk" "$INSTDIR\DatioLabs.exe" "" "$INSTDIR\DatioLabs.exe" 0
  CreateShortcut "$SMPROGRAMS\DatioLabs\Desinstalar.lnk" "$INSTDIR\uninstall.exe" "" "$INSTDIR\uninstall.exe" 0
  
  ; Acceso directo en Escritorio
  CreateShortcut "$DESKTOP\DatioLabs.lnk" "$INSTDIR\DatioLabs.exe" "" "$INSTDIR\DatioLabs.exe" 0
SectionEnd

Section -Post
  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Function .onInit
  ; Inicialización sin chequeos de flags espurios
FunctionEnd

Function un.onInit
  ; Cerrar instancia si está abierta
  FindWindow $0 "DatioLabs"
  ${If} $0 != 0
    MessageBox MB_ICONEXCLAMATION|MB_OK "Cierre DatioLabs antes de desinstalar."
    Abort
  ${EndIf}
FunctionEnd

Function un.onUninstSuccess
  HideWindow
  MessageBox MB_ICONINFORMATION|MB_OK "DatioLabs ha sido desinstalado correctamente de su equipo."
FunctionEnd

Section Uninstall
  ; 1. Detener procesos huérfanos o en ejecución
  nsExec::Exec 'taskkill /F /IM DatioLabs.exe'

  ; 2. Preguntar al usuario si desea eliminar también la base de datos y configuraciones
  MessageBox MB_YESNO|MB_ICONQUESTION "¿Desea eliminar también la base de datos local y todos los datos del negocio?$\n$\nSeleccione 'Sí' para una desinstalación 100% limpia y completa.$\nSeleccione 'No' si desea conservar sus datos para una futura reinstalación." IDNO ConservarDatos

  ; Purgar datos de usuario, base de datos Sled, cachés y WebView2
  RMDir /r "$LOCALAPPDATA\DatioLabs"
  RMDir /r "$APPDATA\com.datiolabs.retail"
  RMDir /r "$LOCALAPPDATA\com.datiolabs.retail"
  RMDir /r "$LOCALAPPDATA\datiolabs_db"

ConservarDatos:
  ; 3. Eliminar archivos de la aplicación
  RMDir /r "$INSTDIR"
  
  ; 4. Eliminar accesos directos
  Delete "$DESKTOP\DatioLabs.lnk"
  RMDir /r "$SMPROGRAMS\DatioLabs"
  
  ; 5. Eliminar entradas del registro de Windows
  DeleteRegKey HKCU "${PRODUCT_UNINST_KEY}"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\App Paths\DatioLabs.exe"
  DeleteRegKey HKCU "Software\DatioLabs"
SectionEnd
