; Windows installer for unskilled browser, built by scripts/package.sh:
;   makensis -DVERSION=<x.y.z> -DBUNDLE=<unpacked bundle folder> -DOUTFILE=<setup.exe> installer.nsi
;
; A per-user install (no admin) into %LOCALAPPDATA%\Programs\unskilled-browser, laid out like the
; Linux install so the browser can update itself (crates/shell/src/updater.rs):
;   versions\<version>\   one folder per version
;   current               a junction to the version to run; shortcuts go through it
; A new version (from this installer or the browser's own updater) goes into its own folder and
; `current` flips to it, so a running browser keeps its files and offers "update · restart".
; The vault (%LOCALAPPDATA%\unskilled-browser) is the user's data and is never touched.

Unicode true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!ifndef VERSION
  !error "pass -DVERSION=, -DBUNDLE= and -DOUTFILE="
!endif
!ifndef BUNDLE
  !error "pass -DVERSION=, -DBUNDLE= and -DOUTFILE="
!endif
!ifndef OUTFILE
  !error "pass -DVERSION=, -DBUNDLE= and -DOUTFILE="
!endif

!define APP "unskilled browser"
!define KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\unskilled-browser"
!define ICON "${__FILEDIR__}\..\assets\icons\unskilled-browser.ico"

Name "${APP}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\unskilled-browser"
BrandingText " "

VIProductVersion "${VERSION}.0"
VIAddVersionKey "ProductName" "${APP}"
VIAddVersionKey "FileDescription" "${APP} setup"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "GPL-3.0-or-later"

!include "MUI2.nsh"
!define MUI_ICON "${ICON}"
!define MUI_UNICON "${ICON}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\current\unskilled.exe"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${APP}"
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section
  SetOutPath "$INSTDIR\versions\${VERSION}"
  File /r "${BUNDLE}\*"

  ; Point `current` at this version: a new junction beside it, swapped in. Removing a junction
  ; removes the link only, never the version folder behind it.
  SetOutPath "$INSTDIR"
  RMDir "$INSTDIR\current.new"
  nsExec::ExecToLog 'cmd /c mklink /J "$INSTDIR\current.new" "$INSTDIR\versions\${VERSION}"'
  Pop $0
  StrCmp $0 "0" +2
    Abort "Couldn't link $INSTDIR\current ($0)."
  RMDir "$INSTDIR\current"
  ClearErrors
  Rename "$INSTDIR\current.new" "$INSTDIR\current"
  IfErrors 0 +2
    Abort "Couldn't switch $INSTDIR\current to ${VERSION}."

  CreateShortCut "$SMPROGRAMS\${APP}.lnk" "$INSTDIR\current\unskilled.exe"
  CreateShortCut "$DESKTOP\${APP}.lnk" "$INSTDIR\current\unskilled.exe"

  WriteUninstaller "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "${KEY}" "DisplayName" "${APP}"
  WriteRegStr HKCU "${KEY}" "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "${KEY}" "Publisher" "itsArial"
  WriteRegStr HKCU "${KEY}" "URLInfoAbout" "https://github.com/itsArial/unskilled-browser"
  WriteRegStr HKCU "${KEY}" "DisplayIcon" "$INSTDIR\current\unskilled.exe"
  WriteRegStr HKCU "${KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${KEY}" "UninstallString" '"$INSTDIR\uninstall.exe"'
  WriteRegStr HKCU "${KEY}" "QuietUninstallString" '"$INSTDIR\uninstall.exe" /S'
  WriteRegDWORD HKCU "${KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${KEY}" "NoRepair" 1
SectionEnd

; A running browser keeps its DLL open for writing by nobody else.
Function un.onInit
  IfFileExists "$INSTDIR\current\unskilled.dll" 0 done
  ClearErrors
  FileOpen $0 "$INSTDIR\current\unskilled.dll" a
  IfErrors 0 +3
    MessageBox MB_ICONEXCLAMATION "Close ${APP} first, then uninstall it." /SD IDOK
    Abort
  FileClose $0
  done:
FunctionEnd

Section Uninstall
  RMDir "$INSTDIR\current"
  RMDir /r "$INSTDIR\versions"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR"
  Delete "$SMPROGRAMS\${APP}.lnk"
  Delete "$DESKTOP\${APP}.lnk"
  DeleteRegKey HKCU "${KEY}"
SectionEnd
