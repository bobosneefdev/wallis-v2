!macro NSIS_HOOK_PREUNINSTALL
  ${If} $UpdateMode <> 1
  ${AndIf} $PassiveMode <> 1
    MessageBox MB_YESNO|MB_ICONQUESTION|MB_DEFBUTTON2 "Remove the Wallis WSL environment and all of his memories, mods and downloads? Choose No to keep them for reinstalling." /SD IDNO IDNO keep_wallis
    nsExec::ExecToLog 'wsl.exe --terminate Wallis'
    nsExec::ExecToLog 'wsl.exe --unregister Wallis'
    keep_wallis:
  ${EndIf}
!macroend
