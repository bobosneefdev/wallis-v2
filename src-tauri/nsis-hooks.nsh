!macro NSIS_HOOK_PREUNINSTALL
  MessageBox MB_YESNO|MB_ICONQUESTION "Remove the Wallis WSL environment and all of his memories, mods and downloads? Choose No to keep them for reinstalling." IDNO keep_wallis
  nsExec::ExecToLog 'wsl.exe --terminate Wallis'
  nsExec::ExecToLog 'wsl.exe --unregister Wallis'
  keep_wallis:
!macroend
