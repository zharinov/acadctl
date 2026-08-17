(defun actl:_dwg-path (/ name)
  (if (= (getvar "DWGTITLED") 1)
    (progn
      (setq name (getvar "DWGNAME"))
      (strcat (getvar "DWGPREFIX") name))))

(defun actl:_read-dwg ()
  (list
    (cons 'name (getvar "DWGNAME"))
    (cons 'path (actl:_dwg-path))
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

(defun actl:dwg (/ outcome)
  (setq outcome
        (vl-catch-all-apply
          'actl:_read-dwg
          '()))
  (if (vl-catch-all-error-p outcome)
    (actl:_err
      'read-failed
      'drawing
      (vl-catch-all-error-message outcome))
    (actl:_ok outcome)))
