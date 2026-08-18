((lambda (/ collect directory file files files-and-directories)
   (setq directory
         (vl-filename-directory (findfile "loader.lsp")))
   (setq files-and-directories 0)

   (setq collect
         '(lambda (directory / entry extension files path)
            (foreach entry
                     (vl-directory-files
                       directory
                       nil
                       files-and-directories)
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
