(setq acadctl:*loader-directory*
  (vl-filename-directory (findfile "loader.lsp")))

(arxload (strcat acadctl:*loader-directory* "/acadctl-plugin.bundle"))
(load (strcat acadctl:*loader-directory* "/driver.lsp"))

(setq acadctl:*loader-directory* nil)
(princ)
