!macro CUSTOM_INSTALL_AFTER_INSTALL
  CreateShortcut "$SMPROGRAMS\LarkNotes Quick Note.lnk" "$INSTDIR\LarkNotes.exe" "--quick-note"
!macroend

!macro CUSTOM_UNINSTALL
  Delete "$SMPROGRAMS\LarkNotes Quick Note.lnk"
!macroend
