(setq actl:*loader-directory*
  (vl-filename-directory (findfile "loader.lsp")))

(arxload (strcat actl:*loader-directory* "/acadctl-plugin.bundle"))

(defun actl:_loader-files
  (directory / entry extension files path)
  (foreach entry (vl-directory-files directory nil 0)
    (if (and (/= entry ".") (/= entry ".."))
      (progn
        (setq path (strcat directory "/" entry))
        (cond
          ((vl-file-directory-p path)
           (setq files
                 (append (actl:_loader-files path) files)))
          ((and (setq extension (vl-filename-extension path))
                (= (strcase extension) ".LSP"))
           (setq files (cons path files)))))))
  files)

((lambda (files / file)
   (setq actl:_loader-files nil)
   (foreach file (vl-sort files '<)
     (load file)))
 (actl:_loader-files (strcat actl:*loader-directory* "/lisp")))

(setq actl:*loader-directory* nil)
(princ)
