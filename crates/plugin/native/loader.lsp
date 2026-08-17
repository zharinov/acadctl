(setq actl:*loader-directory*
  (vl-filename-directory (findfile "loader.lsp")))

(arxload (strcat actl:*loader-directory* "/acadctl-plugin.bundle"))
(load (strcat actl:*loader-directory* "/driver.lsp"))

(setq actl:*loader-directory* nil)
(princ)
