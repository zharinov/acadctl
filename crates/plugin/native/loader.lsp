((lambda (/ collect directory file files)
   (setq directory
         (vl-filename-directory (findfile "loader.lsp")))

   (setq collect
         '(lambda (directory / entry extension files path)
            (foreach entry (vl-directory-files directory nil 0)
              (if (and (/= entry ".") (/= entry ".."))
                (progn
                  (setq path (strcat directory "/" entry))
                  (cond
                    ((vl-file-directory-p path)
                     (setq files
                           (append
                             (apply collect (list path))
                             files)))
                    ((and (setq extension (vl-filename-extension path))
                          (= (strcase extension) ".LSP"))
                     (setq files (cons path files)))))))
            files))

   (setq files
         (apply collect (list (strcat directory "/lisp"))))
   (foreach file (vl-sort files '<)
     (load file))

   (arxload (strcat directory "/acadctl-plugin.bundle"))
   (princ)))
