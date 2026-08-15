(setq acadctl:bundle-directory
  (vl-filename-directory (findfile "acadctl-loader.lsp")))
(arxload (strcat acadctl:bundle-directory "/acadctl-plugin.bundle"))
(load (strcat acadctl:bundle-directory "/execution-driver.lsp"))
(setq acadctl:bundle-directory nil)
(princ)
