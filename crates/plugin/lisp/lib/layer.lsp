(defun actl:layers
  (/ collect counts dxf-aci-code dxf-color-book-code dxf-flags-code dxf-handle-code dxf-linetype-code dxf-lineweight-code dxf-material-code dxf-name-code dxf-plot-code dxf-plot-style-code dxf-transparency-code dxf-true-color-code entity-result entities layer-record layers outcome pointer-handle)
  (setq dxf-aci-code 62)
  (setq dxf-color-book-code 430)
  (setq dxf-flags-code 70)
  (setq dxf-handle-code 5)
  (setq dxf-linetype-code 6)
  (setq dxf-lineweight-code 370)
  (setq dxf-material-code 347)
  (setq dxf-name-code 2)
  (setq dxf-plot-code 290)
  (setq dxf-plot-style-code 390)
  (setq dxf-transparency-code 440)
  (setq dxf-true-color-code 420)

  (setq pointer-handle
        '(lambda (entity / data value)
           (if (and entity
                    (setq data (entget entity))
                    (setq value (cdr (assoc dxf-handle-code data))))
             (strcase value))))

  (setq counts
        '(lambda (name / color current entity key linetype lineweight overrides records row)
           (setq overrides
                 (list
                   (cons 'color 0)
                   (cons 'linetype 0)
                   (cons 'lineweight 0)))
           (foreach entity entities
             (if (= (cdr (assoc 'layer entity)) name)
               (progn
                 (setq key
                       (list
                         (cdr (assoc 'layout entity))
                         (cdr (assoc 'type entity))))
                 (if (setq current (assoc key records))
                   (setq records
                         (subst
                           (cons key (1+ (cdr current)))
                           current
                           records))
                   (setq records (cons (cons key 1) records)))
                 (if (eq (cdr (assoc 'color-source entity)) 'explicit)
                   (progn
                     (setq color (assoc 'color overrides))
                     (setq overrides
                           (subst
                             (cons 'color (1+ (cdr color)))
                             color
                             overrides))))
                 (if (eq (cdr (assoc 'linetype-source entity)) 'explicit)
                   (progn
                     (setq linetype (assoc 'linetype overrides))
                     (setq overrides
                           (subst
                             (cons 'linetype (1+ (cdr linetype)))
                             linetype
                             overrides))))
                 (if (eq (cdr (assoc 'lineweight-source entity)) 'explicit)
                   (progn
                     (setq lineweight (assoc 'lineweight overrides))
                     (setq overrides
                           (subst
                             (cons 'lineweight (1+ (cdr lineweight)))
                             lineweight
                             overrides)))))))
           (setq records
                 (vl-sort
                   records
                   '(lambda (left right / left-key right-key)
                      (setq left-key (car left))
                      (setq right-key (car right))
                      (if (= (car left-key) (car right-key))
                        (< (cadr left-key) (cadr right-key))
                        (< (car left-key) (car right-key))))))
           (setq current nil)
           (foreach row records
             (setq current
                   (cons
                     (list
                       (cons 'layout (car (car row)))
                       (cons 'type (cadr (car row)))
                       (cons 'count (cdr row)))
                     current)))
           (list
             (cons 'counts (reverse current))
             (cons 'overrides overrides))))

  (setq layer-record
        '(lambda (data / aci flags inventory name)
           (setq aci (cdr (assoc dxf-aci-code data)))
           (setq flags (cdr (assoc dxf-flags-code data)))
           (setq name (cdr (assoc dxf-name-code data)))
           (setq inventory (apply counts (list name)))
           (append
             (list
               (cons 'name name)
               (cons
                 'handle
                 (strcase (cdr (assoc dxf-handle-code data))))
               (cons 'flags flags)
               (cons 'off (if (and aci (< aci 0)) T nil))
               (cons 'frozen (if (/= (logand flags 1) 0) T nil))
               (cons
                 'frozen-in-new-viewports
                 (if (/= (logand flags 2) 0) T nil))
               (cons 'locked (if (/= (logand flags 4) 0) T nil))
               (cons 'dependent (if (/= (logand flags 16) 0) T nil))
               (cons 'referenced (if (/= (logand flags 64) 0) T nil))
               (cons 'aci aci)
               (cons 'true-color (cdr (assoc dxf-true-color-code data)))
               (cons 'color-book (cdr (assoc dxf-color-book-code data)))
               (cons 'linetype (cdr (assoc dxf-linetype-code data)))
               (cons 'lineweight (cdr (assoc dxf-lineweight-code data)))
               (cons
                 'transparency
                 (cdr (assoc dxf-transparency-code data)))
               (cons 'plot (cdr (assoc dxf-plot-code data)))
               (cons
                 'plot-style-handle
                 (apply
                   pointer-handle
                   (list (cdr (assoc dxf-plot-style-code data)))))
               (cons
                 'material-handle
                 (apply
                   pointer-handle
                   (list (cdr (assoc dxf-material-code data))))))
             inventory)))

  (setq collect
        '(lambda (/ data records reference state)
           (setq data (tblnext "LAYER" T))
           (while (and data (null state))
             (setq reference
                   (actl:dxf
                     (tblobjname
                       "LAYER"
                       (cdr (assoc dxf-name-code data)))))
             (if (or (null reference)
                     (eq (car reference) 'error))
               (setq state
                     (if reference
                       reference
                       (actl:err
                         (list "A layer table record is unavailable"))))
               (setq records
                     (cons
                       (cdr (assoc 'value (cdr reference)))
                       records)))
             (setq data (tblnext "LAYER")))
           (if state
             state
             (progn
               (setq records
                     (vl-sort
                       records
                       '(lambda (left right)
                          (<
                            (cdr (assoc dxf-name-code left))
                            (cdr (assoc dxf-name-code right))))))
               (foreach data records
                 (setq layers
                       (cons
                         (apply layer-record (list data))
                         layers)))
               (actl:ok
                 (list
                   (cons 'items (reverse layers))))))))

  (setq outcome
        (vl-catch-all-apply
          '(lambda ()
             (setq entity-result (actl:find nil))
             (if (eq (car entity-result) 'error)
               entity-result
               (progn
                 (setq entities
                       (cdr
                         (assoc
                           'items
                           (cdr entity-result))))
                 (apply collect '()))))
          '()))
  (if (vl-catch-all-error-p outcome)
    (actl:err
      (list
        (strcat
          "Could not inspect layers: "
          (vl-catch-all-error-message outcome))))
    outcome))
