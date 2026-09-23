; Voyis Network Speed - Windows installer (NSIS)
; Build from the installer directory:
;   cd installer && makensis voyis-network-speed.nsi
; Produces: installer/VoyisNetworkSpeed-Setup.exe
; (File paths below are relative to this script's directory.)

!include "MUI2.nsh"
!include "x64.nsh"

Name "Voyis Network Speed"
OutFile "VoyisNetworkSpeed-Setup.exe"
InstallDir "$PROGRAMFILES64\Voyis Network Speed"
RequestExecutionLevel admin

!define APP_VERSION "0.1.0"
!define APP_PUBLISHER "Voyis Imaging Inc."

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Section "Voyis Network Speed" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"

  ; Application binary (cross-compiled on Linux with
  ; cargo build --release --target x86_64-pc-windows-gnu)
  File "..\target\x86_64-pc-windows-gnu\release\voyis-network-speed.exe"

  ; Bundled iperf3 (BSD-3-Clause, see LICENSE.iperf3.txt).
  ; The app prefers this copy at startup; the user can still point
  ; the app at a different iperf3 via Browse in the UI.
  File "third-party\iperf3.exe"
  File "third-party\cygwin1.dll"
  File "third-party\LICENSE.iperf3.txt"

  File "..\README.md"

  WriteUninstaller "$INSTDIR\Uninstall.exe"

  CreateDirectory "$SMPROGRAMS\Voyis Network Speed"
  CreateShortcut "$SMPROGRAMS\Voyis Network Speed\Voyis Network Speed.lnk" \
    "$INSTDIR\voyis-network-speed.exe"
  CreateShortcut "$SMPROGRAMS\Voyis Network Speed\Uninstall.lnk" \
    "$INSTDIR\Uninstall.exe"

  WriteRegStr HKLM \
    "Software\Microsoft\Windows\CurrentVersion\Uninstall\VoyisNetworkSpeed" \
    "DisplayName" "Voyis Network Speed"
  WriteRegStr HKLM \
    "Software\Microsoft\Windows\CurrentVersion\Uninstall\VoyisNetworkSpeed" \
    "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKLM \
    "Software\Microsoft\Windows\CurrentVersion\Uninstall\VoyisNetworkSpeed" \
    "Publisher" "${APP_PUBLISHER}"
  WriteRegStr HKLM \
    "Software\Microsoft\Windows\CurrentVersion\Uninstall\VoyisNetworkSpeed" \
    "UninstallString" "$INSTDIR\Uninstall.exe"
  WriteRegDWORD HKLM \
    "Software\Microsoft\Windows\CurrentVersion\Uninstall\VoyisNetworkSpeed" \
    "NoModify" 1
  WriteRegDWORD HKLM \
    "Software\Microsoft\Windows\CurrentVersion\Uninstall\VoyisNetworkSpeed" \
    "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\voyis-network-speed.exe"
  Delete "$INSTDIR\iperf3.exe"
  Delete "$INSTDIR\cygwin1.dll"
  Delete "$INSTDIR\LICENSE.iperf3.txt"
  Delete "$INSTDIR\README.md"
  Delete "$INSTDIR\Uninstall.exe"
  RMDir "$INSTDIR"

  Delete "$SMPROGRAMS\Voyis Network Speed\Voyis Network Speed.lnk"
  Delete "$SMPROGRAMS\Voyis Network Speed\Uninstall.lnk"
  RMDir "$SMPROGRAMS\Voyis Network Speed"

  DeleteRegKey HKLM \
    "Software\Microsoft\Windows\CurrentVersion\Uninstall\VoyisNetworkSpeed"
SectionEnd
