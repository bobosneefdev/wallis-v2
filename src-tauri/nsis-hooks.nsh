; Wallis keeps most of its data inside a WSL2 distribution, which the NSIS template knows
; nothing about. When the user ticks "delete application data" at uninstall, unregister the
; distro too — that is what permanently deletes Hermes' home and the Mnemosyne memory.
!include x64.nsh

!macro NSIS_HOOK_POSTUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    DetailPrint "Removing the Wallis WSL2 distribution and its memory..."
    ; The uninstaller is a 32-bit process, so $SYSDIR would redirect to SysWOW64 (no wsl.exe).
    ${DisableX64FSRedirection}
    nsExec::ExecToLog '"$SYSDIR\wsl.exe" --unregister Wallis'
    Pop $0
    ${EnableX64FSRedirection}
    ${If} $0 <> 0
      DetailPrint "WSL could not unregister Wallis (exit $0). Run: wsl --unregister Wallis"
    ${EndIf}
    ; wsl.exe deletes the distro's disk image; this clears the folder Wallis imported it into.
    SetShellVarContext current
    RmDir /r "$LOCALAPPDATA\Wallis"
  ${EndIf}
!macroend
