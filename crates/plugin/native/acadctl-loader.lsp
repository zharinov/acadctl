(setq acadctl:*loader-directory*
  (vl-filename-directory (findfile "acadctl-loader.lsp")))
(arxload (strcat acadctl:*loader-directory* "/acadctl-plugin.bundle"))
(load (strcat acadctl:*loader-directory* "/execution-driver.lsp"))
(setq acadctl:*loader-directory* nil)
(princ)
