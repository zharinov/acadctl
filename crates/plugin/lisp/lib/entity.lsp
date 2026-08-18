(defun actl:dxf (subject / dxf-handle-code outcome)
  (setq dxf-handle-code 5)
  (if (not (or (eq (type subject) 'STR)
               (eq (type subject) 'ENAME)))
    (actl:err
      (list
        '(code . invalid-subject)
        (cons 'subject subject)
        '(message . "Expected a handle string or entity name")))
    (progn
      (setq outcome
            (vl-catch-all-apply
              '(lambda (/ data ename handle)
                 (setq ename
                       (if (eq (type subject) 'STR)
                         (handent subject)
                         subject))
                 (if ename
                   (setq data (entget ename)))

                 (cond
                   ((null data)
                    nil)
                   ((null
                      (setq handle
                            (cdr (assoc dxf-handle-code data))))
                    (actl:err
                      (list
                        '(code . missing-handle)
                        (cons 'subject subject)
                        '(message . "The object has no DXF handle"))))
                   (T
                    (actl:ok
                      (list
                        (cons 'handle (strcase handle))
                        (cons 'value data))))))
              '()))

      (if (vl-catch-all-error-p outcome)
        (actl:err
          (list
            '(code . read-failed)
            (cons 'subject subject)
            (cons 'message (vl-catch-all-error-message outcome))))
        outcome))))

(defun actl:entities
  (source /
   aci-by-block
   aci-by-layer
   dxf-aci-code
   dxf-block-first-entity-code
   dxf-color-book-code
   dxf-control-string-code
   dxf-dictionary-key-code
   dxf-entity-name-code
   dxf-handle-code
   dxf-hard-pointer-code
   dxf-layer-code
   dxf-layout-code
   dxf-lineweight-code
   dxf-linetype-code
   dxf-name-code
   dxf-owner-code
   dxf-soft-owner-code
   dxf-true-color-code
   dxf-type-code
   entities-from-refs
   entity-item
   handle
   layout-error
   layout-entities
   layout-name
   layout-reference
   layout-start
   literal-pattern
   outcome
   owner-handle
   owner-root
   lineweight-by-block
   lineweight-by-layer
   lineweight-default
   source-entities)
  (setq aci-by-block 0)
  (setq aci-by-layer 256)
  (setq dxf-aci-code 62)
  (setq dxf-block-first-entity-code -2)
  (setq dxf-color-book-code 430)
  (setq dxf-control-string-code 102)
  (setq dxf-dictionary-key-code 3)
  (setq dxf-entity-name-code -1)
  (setq dxf-handle-code 5)
  (setq dxf-hard-pointer-code 340)
  (setq dxf-layer-code 8)
  (setq dxf-layout-code 410)
  (setq dxf-lineweight-code 370)
  (setq dxf-linetype-code 6)
  (setq dxf-name-code 2)
  (setq dxf-owner-code 330)
  (setq dxf-soft-owner-code 350)
  (setq dxf-true-color-code 420)
  (setq dxf-type-code 0)
  (setq lineweight-by-block -2)
  (setq lineweight-by-layer -1)
  (setq lineweight-default -3)

  (setq handle
        '(lambda (subject / data)
           (cond
             ((eq (type subject) 'STR)
              (strcase subject))
             ((and (eq (type subject) 'ENAME)
                   (setq data (entget subject))
                   (assoc dxf-handle-code data))
              (strcase (cdr (assoc dxf-handle-code data)))))))

  (setq owner-handle
        '(lambda (data / depth owner pair)
           (setq depth 0)
           (foreach pair data
             (cond
               ((and (= (car pair) dxf-control-string-code)
                     (/= (cdr pair) "}"))
                (setq depth (1+ depth)))
               ((and (= (car pair) dxf-control-string-code)
                     (= (cdr pair) "}"))
                (setq depth (1- depth)))
               ((and (= depth 0)
                     (= (car pair) dxf-owner-code)
                     (null owner))
                (setq owner (cdr pair)))))
           owner))

  (setq layout-name
        '(lambda (requested / candidate found)
           (if (= (strcase requested) "MODEL")
             (setq found "Model")
             (foreach candidate (layoutlist)
               (if (= (strcase candidate) (strcase requested))
                 (setq found candidate))))
           found))

  (setq layout-reference
        '(lambda (name / found key pair)
           (foreach pair
                    (dictsearch (namedobjdict) "ACAD_LAYOUT")
             (cond
               ((= (car pair) dxf-dictionary-key-code)
                (setq key (cdr pair)))
               ((= (car pair) dxf-soft-owner-code)
                (if (and key
                         (= (strcase key) (strcase name)))
                  (setq found (cdr pair)))
                (setq key nil))))
           found))

  (setq literal-pattern
        '(lambda (value / character index result)
           (setq index 1)
           (setq result "")
           (while (<= index (strlen value))
             (setq character (substr value index 1))
             (setq result
                   (strcat
                     result
                     (if (wcmatch character "[A-Za-z0-9]")
                       character
                       (strcat "`" character))))
             (setq index (1+ index)))
           result))

  (setq owner-root
        '(lambda (entity / data root seen)
           (while (and entity
                       (null root)
                       (not (member entity seen)))
             (setq seen (cons entity seen))
             (setq data (entget entity))
             (if (and data
                      (= (cdr (assoc dxf-type-code data)) "BLOCK_RECORD"))
               (setq root entity)
               (setq entity
                     (apply owner-handle (list data)))))
           root))

  (setq layout-error
        '(lambda (requested)
           (actl:err
             (list
               '(code . read-failed)
               (cons 'subject requested)
               '(message . "Layout ownership data is inconsistent")))))

  (setq layout-start
        '(lambda (layout set / block block-data data first target)
           (if (and
                 (setq target
                       (apply
                         owner-root
                         (list (ssname set 0))))
                 (setq data (entget target))
                 (eq (cdr (assoc dxf-hard-pointer-code data)) layout)
                 (setq block
                       (tblobjname
                         "BLOCK"
                         (cdr (assoc dxf-name-code data))))
                 (setq block-data (entget block))
                 (= (cdr (assoc dxf-type-code block-data)) "BLOCK")
                 (eq
                   (apply owner-handle (list block-data))
                   target)
                 (eq
                   (type
                     (setq first
                           (cdr
                             (assoc
                               dxf-block-first-entity-code
                               block-data))))
                   'ENAME))
             (list target first))))

  ;; The layout dictionary supplies identity without reading the LAYOUT
  ;; object. Entity ownership selects the layout and preserves its chain.
  (setq layout-entities
        '(lambda
           (requested /
            current
            data
            done
            items
            layout
            name
            observed
            root
            seen
            set
            start
            target)
           (if (setq name (apply layout-name (list requested)))
             (if (null
                   (setq layout
                         (apply layout-reference (list name))))
               (apply layout-error (list requested))
               (progn
                 (setq set
                       (ssget
                         "_X"
                         (list
                           (cons
                             dxf-layout-code
                             (apply
                               literal-pattern
                               (list name))))))
                 (if (null set)
                   (actl:ok (list (cons 'items nil)))
                   (progn
                     (setq start
                           (apply layout-start (list layout set)))
                     (if start
                       (progn
                         (setq target (car start))
                         (setq current (cadr start)))
                       (setq done 'failed))
                     (while (and current (null done))
                       (if (member current seen)
                         (setq done 'failed)
                         (progn
                           (setq seen (cons current seen))
                           (setq data (entget current))
                           (cond
                             ((null data)
                              (setq done 'failed))
                             ((= (cdr (assoc dxf-type-code data)) "ENDBLK")
                              (setq done T))
                             ((null
                                (setq root
                                      (apply
                                        owner-root
                                        (list current))))
                              (setq done 'failed))
                             ((not (eq root target))
                              (setq done T))
                             (T
                              (setq items (cons current items))
                              (if (ssmemb current set)
                                (setq observed
                                      (1+
                                        (if observed observed 0))))
                              (setq current (entnext current)))))))
                     (if (or (eq done 'failed)
                             (/=
                               (if observed observed 0)
                               (sslength set)))
                       (apply layout-error (list requested))
                       (actl:ok
                         (list
                           (cons 'items (reverse items))))))))))))

  (setq source-entities
        '(lambda (source / data groups index items pair)
           (cond
             ((eq source 'model)
              (apply layout-entities (list "Model")))
             ((and (eq (type source) 'LIST)
                   (eq (car source) 'layout)
                   (eq (type (cdr source)) 'STR))
              (apply layout-entities (list (cdr source))))
             ((and (eq (type source) 'LIST)
                   (eq (car source) 'group)
                   (eq (type (cdr source)) 'STR))
              (if (and
                    (setq groups
                          (dictsearch (namedobjdict) "ACAD_GROUP"))
                    (setq data
                          (dictsearch
                            (cdr (assoc dxf-entity-name-code groups))
                            (cdr source))))
                (progn
                  (foreach pair data
                    (if (= (car pair) dxf-hard-pointer-code)
                      (setq items (cons (cdr pair) items))))
                  (actl:ok
                    (list
                      (cons 'items (reverse items)))))))
             ((eq (type source) 'PICKSET)
              (setq index 0)
              (while (< index (sslength source))
                (setq items (cons (ssname source index) items))
                (setq index (1+ index)))
              (actl:ok
                (list
                  (cons 'items (reverse items)))))
             ((or (null source) (eq (type source) 'LIST))
              (actl:ok
                (list
                  (cons 'items source))))
             (T
              (actl:err
                (list
                  '(code . invalid-source)
                  (cons 'subject source)
                  '(message . "Expected model, a layout, a group, a pickset, or references")))))))

  (setq entity-item
        '(lambda (reference index / aci data linetype lineweight owner)
           (setq data (cdr (assoc 'value (cdr reference))))
           (setq owner
                 (apply owner-handle (list data)))
           (setq aci (cdr (assoc dxf-aci-code data)))
           (setq linetype (cdr (assoc dxf-linetype-code data)))
           (setq lineweight (cdr (assoc dxf-lineweight-code data)))
           (list
             (cons 'handle (cdr (assoc 'handle (cdr reference))))
             (cons 'type (cdr (assoc dxf-type-code data)))
             (cons 'owner-handle (apply handle (list owner)))
             (cons 'layout (cdr (assoc dxf-layout-code data)))
             (cons 'layer (cdr (assoc dxf-layer-code data)))
             (cons
               'color-source
               (cond
                 ((or (assoc dxf-true-color-code data)
                      (assoc dxf-color-book-code data))
                  'explicit)
                 ((or (null aci) (= aci aci-by-layer)) 'by-layer)
                 ((= aci aci-by-block) 'by-block)
                 (T 'explicit)))
             (cons
               'linetype-source
               (cond
                 ((or (null linetype)
                      (= (strcase linetype) "BYLAYER"))
                  'by-layer)
                 ((= (strcase linetype) "BYBLOCK") 'by-block)
                 (T 'explicit)))
             (cons
               'lineweight-source
               (cond
                 ((or (null lineweight)
                      (= lineweight lineweight-by-layer))
                  'by-layer)
                 ((= lineweight lineweight-by-block) 'by-block)
                 ((= lineweight lineweight-default) 'default)
                 (T 'explicit)))
             (cons 'observed-index index))))

  (setq entities-from-refs
        '(lambda (refs source / index item items reference state)
           (setq index 0)
           (while (and refs (null state))
             (setq reference (actl:dxf (car refs)))
             (cond
               ((null reference)
                (setq state
                      (actl:err
                        (list
                          (if (and
                                (eq (type source) 'LIST)
                                (eq (car source) 'group)
                                (eq (type (cdr source)) 'STR))
                            (strcat
                              "Could not read member "
                              (itoa index)
                              " of group "
                              (cdr source))
                            (strcat
                              "Could not read reference at index "
                              (itoa index)))))))
               ((eq (car reference) 'error)
                (setq state reference))
               (T
                (setq item
                      (apply entity-item (list reference index)))
                (setq items (cons item items))
                (setq index (1+ index))
                (setq refs (cdr refs)))))

           (cond
             (state state)
             (T
              (actl:ok
                (list
                  (cons 'items (reverse items))))))))

  (setq outcome
        (vl-catch-all-apply source-entities (list source)))
  (cond
    ((vl-catch-all-error-p outcome)
     (actl:err
       (list
         '(code . read-failed)
         (cons 'subject source)
         (cons 'message (vl-catch-all-error-message outcome)))))
    ((or (null outcome) (eq (car outcome) 'error))
     outcome)
    (T
     (setq outcome
           (vl-catch-all-apply
             entities-from-refs
             (list
               (cdr (assoc 'items (cdr outcome)))
               source)))
     (if (vl-catch-all-error-p outcome)
       (actl:err
         (list
           '(code . read-failed)
           (cons 'subject source)
           (cons 'message (vl-catch-all-error-message outcome))))
       outcome))))

(defun actl:find (filter / less outcome)
  (if (not (or (null filter) (eq (type filter) 'LIST)))
    (actl:err
      (list
        '(code . invalid-filter)
        (cons 'subject filter)
        '(message . "Expected a DXF filter list")))
    (progn
      (setq less
            '(lambda (left right / a b)
               (setq a
                     (vl-string-left-trim
                       "0"
                       (cdr (assoc 'handle left))))
               (setq b
                     (vl-string-left-trim
                       "0"
                       (cdr (assoc 'handle right))))
               (cond
                 ((< (strlen a) (strlen b)) T)
                 ((> (strlen a) (strlen b)) nil)
                 (T (< a b)))))

      (setq outcome
            (vl-catch-all-apply
              '(lambda (/ index items output result set)
                 (setq set
                       (if filter
                         (ssget "_X" filter)
                         (ssget "_X")))
                 (if set
                   (progn
                     (setq result (actl:entities set))
                     (if (or (null result)
                             (eq (car result) 'error))
                       result
                       (progn
                         (setq items
                               (vl-sort
                                 (cdr (assoc 'items (cdr result)))
                                 less))
                         (setq index 0)
                         (foreach item items
                           (setq output
                                 (cons
                                   (subst
                                     (cons 'observed-index index)
                                     (assoc 'observed-index item)
                                     item)
                                   output))
                           (setq index (1+ index)))
                         (actl:ok
                           (list
                             (cons 'items (reverse output)))))))
                   (actl:ok
                     (list
                       (cons 'items nil)))))
              '()))

      (if (vl-catch-all-error-p outcome)
        (actl:err
          (list
            '(code . invalid-filter)
            (cons 'subject filter)
            (cons 'message (vl-catch-all-error-message outcome))))
        outcome))))

(defun actl:entity
  (subject /
   aci-by-block
   aci-by-layer
   color-source
   dxf-aci-code
   dxf-color-book-code
   dxf-control-string-code
   dxf-extension-dictionary-code
   dxf-handle-code
   dxf-layer-code
   dxf-layout-code
   dxf-lineweight-code
   dxf-linetype-code
   dxf-owner-code
   dxf-transparency-code
   dxf-true-color-code
   dxf-type-code
   dxf-visibility-code
   entity-invisible
   extension-dictionary
   handle
   lineweight-by-block
   lineweight-by-layer
   lineweight-default
   lineweight-source
   linetype-source
   outcome
   owner-handle
   reference
   transparency-by-block
   transparency-by-layer
   transparency-method-shift
   transparency-source
   xdata-group-code
   xdata-apps)
  (setq aci-by-block 0)
  (setq aci-by-layer 256)
  (setq dxf-aci-code 62)
  (setq dxf-color-book-code 430)
  (setq dxf-control-string-code 102)
  (setq dxf-extension-dictionary-code 360)
  (setq dxf-handle-code 5)
  (setq dxf-layer-code 8)
  (setq dxf-layout-code 410)
  (setq dxf-lineweight-code 370)
  (setq dxf-linetype-code 6)
  (setq dxf-owner-code 330)
  (setq dxf-transparency-code 440)
  (setq dxf-true-color-code 420)
  (setq dxf-type-code 0)
  (setq dxf-visibility-code 60)
  (setq entity-invisible 1)
  (setq lineweight-by-block -2)
  (setq lineweight-by-layer -1)
  (setq lineweight-default -3)
  (setq transparency-by-block 1)
  (setq transparency-by-layer 0)
  (setq transparency-method-shift -24)
  (setq xdata-group-code -3)

  (setq reference (actl:dxf subject))
  (if (or (null reference) (eq (car reference) 'error))
    reference
    (progn
      (setq handle
            '(lambda (subject / data)
               (cond
                 ((eq (type subject) 'STR)
                  (strcase subject))
                 ((and (eq (type subject) 'ENAME)
                       (setq data (entget subject))
                       (assoc dxf-handle-code data))
                  (strcase
                    (cdr (assoc dxf-handle-code data)))))))

      (setq owner-handle
            '(lambda (data / depth owner pair)
               (setq depth 0)
               (foreach pair data
                 (cond
                   ((and (= (car pair) dxf-control-string-code)
                         (/= (cdr pair) "}"))
                    (setq depth (1+ depth)))
                   ((and (= (car pair) dxf-control-string-code)
                         (= (cdr pair) "}"))
                    (setq depth (1- depth)))
                   ((and (= depth 0)
                         (= (car pair) dxf-owner-code)
                         (null owner))
                    (setq owner (cdr pair)))))
               owner))

      (setq color-source
            '(lambda (data / aci)
               (setq aci (cdr (assoc dxf-aci-code data)))
               (cond
                 ((or (assoc dxf-true-color-code data)
                      (assoc dxf-color-book-code data))
                  'explicit)
                 ((or (null aci) (= aci aci-by-layer)) 'by-layer)
                 ((= aci aci-by-block) 'by-block)
                 (T 'explicit))))

      (setq linetype-source
            '(lambda (data / value)
               (setq value (cdr (assoc dxf-linetype-code data)))
               (cond
                 ((or (null value) (= (strcase value) "BYLAYER"))
                  'by-layer)
                 ((= (strcase value) "BYBLOCK") 'by-block)
                 (T 'explicit))))

      (setq lineweight-source
            '(lambda (data / value)
               (setq value (cdr (assoc dxf-lineweight-code data)))
               (cond
                 ((or (null value)
                      (= value lineweight-by-layer))
                  'by-layer)
                 ((= value lineweight-by-block) 'by-block)
                 ((= value lineweight-default) 'default)
                 (T 'explicit))))

      (setq transparency-source
                '(lambda (data / method value)
                   (setq value
                         (cdr (assoc dxf-transparency-code data)))
                   (if value
                     (setq method
                           (lsh value transparency-method-shift)))
                   (cond
                     ((or (null method)
                          (= method transparency-by-layer))
                      'by-layer)
                     ((= method transparency-by-block) 'by-block)
                     (T 'explicit))))

          (setq extension-dictionary
                '(lambda (data / active found pair)
                   (foreach pair data
                     (cond
                       ((and (= (car pair) dxf-control-string-code)
                             (= (cdr pair) "{ACAD_XDICTIONARY"))
                        (setq active T))
                       ((and active
                             (= (car pair)
                                dxf-extension-dictionary-code))
                        (setq found T))
                       ((and active
                             (= (car pair) dxf-control-string-code)
                             (= (cdr pair) "}"))
                        (setq active nil))))
                   found))

          (setq xdata-apps
                '(lambda (data / apps pair subgroup)
                   (foreach pair data
                     (if (= (car pair) xdata-group-code)
                       (foreach subgroup (cdr pair)
                         (if (eq (type (car subgroup)) 'STR)
                           (setq
                             apps
                             (cons (car subgroup) apps))))))
                   (reverse apps)))

          (setq outcome
                (vl-catch-all-apply
                  '(lambda (/ data full object-handle owner)
                     (setq object-handle
                           (cdr (assoc 'handle (cdr reference))))
                     (setq data
                           (cdr (assoc 'value (cdr reference))))
                     (setq full
                           (entget (handent object-handle) '("*")))
                     (setq owner
                           (apply owner-handle (list data)))

                     (list
                       (cons 'handle object-handle)
                       (cons 'type (cdr (assoc dxf-type-code data)))
                       (cons
                         'owner-handle
                         (apply handle (list owner)))
                       (cons
                         'layout
                         (cdr (assoc dxf-layout-code data)))
                       (cons 'layer (cdr (assoc dxf-layer-code data)))
                       (cons
                         'visible
                         (not
                           (eq
                             (cdr (assoc dxf-visibility-code data))
                             entity-invisible)))
                       (cons
                         'color-source
                         (apply color-source (list data)))
                       (cons 'aci (cdr (assoc dxf-aci-code data)))
                       (cons
                         'true-color
                         (cdr (assoc dxf-true-color-code data)))
                       (cons
                         'color-book
                         (cdr (assoc dxf-color-book-code data)))
                       (cons
                         'linetype-source
                         (apply linetype-source (list data)))
                       (cons
                         'linetype
                         (cdr (assoc dxf-linetype-code data)))
                       (cons
                         'lineweight-source
                         (apply lineweight-source (list data)))
                       (cons
                         'lineweight
                         (cdr (assoc dxf-lineweight-code data)))
                       (cons
                         'transparency-source
                         (apply transparency-source (list data)))
                       (cons
                         'transparency
                         (cdr (assoc dxf-transparency-code data)))
                       (cons
                         'extension-dictionary
                         (apply extension-dictionary (list data)))
                       (cons
                         'xdata-applications
                         (apply xdata-apps (list full)))))
                  '()))

          (if (vl-catch-all-error-p outcome)
            (actl:err
              (list
                '(code . read-failed)
                (cons 'subject subject)
                (cons 'message (vl-catch-all-error-message outcome))))
            (actl:ok outcome)))))

(defun actl:xdata
  (subject applications / outcome reference xdata-group-code)
  (setq xdata-group-code -3)
  (if (not
        (or
          (eq applications 'all)
          (null applications)
          (and
            (eq (type applications) 'LIST)
            (vl-every
              '(lambda (application)
                 (eq (type application) 'STR))
              applications))))
    (actl:err
      (list
        '(code . invalid-applications)
        (cons 'subject applications)
        '(message . "Expected all or a list of application names")))
    (progn
      (setq reference (actl:dxf subject))
      (if (or (null reference) (eq (car reference) 'error))
        reference
        (if (null applications)
          (actl:ok
            (list
              (cons
                'handle
                (cdr (assoc 'handle (cdr reference))))
              (cons 'value nil)))
          (progn
            (setq outcome
                  (vl-catch-all-apply
                    '(lambda (/ data object-handle pair value)
                       (setq object-handle
                             (cdr (assoc 'handle (cdr reference))))
                       (setq data
                             (entget
                               (handent object-handle)
                               (if (eq applications 'all)
                                 '("*")
                                 applications)))
                       (foreach pair data
                         (if (= (car pair) xdata-group-code)
                           (setq value (cons pair value))))
                       (actl:ok
                         (list
                           (cons 'handle object-handle)
                           (cons 'value (reverse value)))))
                    '()))

            (if (vl-catch-all-error-p outcome)
              (actl:err
                (list
                  '(code . read-failed)
                  (cons 'subject subject)
                  (cons 'message (vl-catch-all-error-message outcome))))
              outcome)))))))
