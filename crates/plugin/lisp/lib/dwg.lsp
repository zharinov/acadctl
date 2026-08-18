(defun actl:dwg (/ outcome)
  (setq outcome
        (vl-catch-all-apply
          '(lambda (/ name path)
             (if (= (getvar "DWGTITLED") 1)
               (progn
                 (setq name (getvar "DWGNAME"))
                 (setq path
                       (strcat (getvar "DWGPREFIX") name))))

             (list
               (cons 'name (getvar "DWGNAME"))
               (cons 'path path)
               (cons 'dbmod (getvar "DBMOD"))
               (cons 'insertion-units (getvar "INSUNITS"))
               (cons 'measurement (getvar "MEASUREMENT"))
               (cons 'layout (getvar "CTAB"))
               (cons 'space
                     (if (= (getvar "CVPORT") 1)
                       'paper
                       'model))
               (cons 'current-layer (getvar "CLAYER"))
               (list
                 'ucs
                 '(coordinates . wcs)
                 (cons 'origin (getvar "UCSORG"))
                 (cons 'x-axis (getvar "UCSXDIR"))
                 (cons 'y-axis (getvar "UCSYDIR")))
               (list
                 'extents
                 '(source . stored)
                 '(coordinates . wcs)
                 (cons 'min (getvar "EXTMIN"))
                 (cons 'max (getvar "EXTMAX")))))
          '()))
  (if (vl-catch-all-error-p outcome)
    (actl:err
      (list
        '(code . read-failed)
        '(subject . drawing)
        (cons 'message (vl-catch-all-error-message outcome))))
    (actl:ok outcome)))
