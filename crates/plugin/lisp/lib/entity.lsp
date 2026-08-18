(defun actl:dxf (subject / outcome)
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
                   ((null (setq handle (cdr (assoc 5 data))))
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
   source-entities)
  (setq handle
        '(lambda (subject / data)
           (cond
             ((eq (type subject) 'STR)
              (strcase subject))
             ((and (eq (type subject) 'ENAME)
                   (setq data (entget subject))
                   (assoc 5 data))
              (strcase (cdr (assoc 5 data)))))))

  (setq owner-handle
        '(lambda (data / depth owner pair)
           (setq depth 0)
           (foreach pair data
             (cond
               ((and (= (car pair) 102)
                     (/= (cdr pair) "}"))
                (setq depth (1+ depth)))
               ((and (= (car pair) 102)
                     (= (cdr pair) "}"))
                (setq depth (1- depth)))
               ((and (= depth 0)
                     (= (car pair) 330)
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
               ((= (car pair) 3)
                (setq key (cdr pair)))
               ((= (car pair) 350)
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
                      (= (cdr (assoc 0 data)) "BLOCK_RECORD"))
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
                 (eq (cdr (assoc 340 data)) layout)
                 (setq block
                       (tblobjname
                         "BLOCK"
                         (cdr (assoc 2 data))))
                 (setq block-data (entget block))
                 (= (cdr (assoc 0 block-data)) "BLOCK")
                 (eq
                   (apply owner-handle (list block-data))
                   target)
                 (eq
                   (type
                     (setq first (cdr (assoc -2 block-data))))
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
                             410
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
                             ((= (cdr (assoc 0 data)) "ENDBLK")
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
                            (cdr (assoc -1 groups))
                            (cdr source))))
                (progn
                  (foreach pair data
                    (if (= (car pair) 340)
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
           (setq aci (cdr (assoc 62 data)))
           (setq linetype (cdr (assoc 6 data)))
           (setq lineweight (cdr (assoc 370 data)))
           (list
             (cons 'handle (cdr (assoc 'handle (cdr reference))))
             (cons 'type (cdr (assoc 0 data)))
             (cons 'owner-handle (apply handle (list owner)))
             (cons 'layout (cdr (assoc 410 data)))
             (cons 'layer (cdr (assoc 8 data)))
             (cons
               'color-source
               (cond
                 ((or (assoc 420 data) (assoc 430 data)) 'explicit)
                 ((or (null aci) (= aci 256)) 'by-layer)
                 ((= aci 0) 'by-block)
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
                 ((or (null lineweight) (= lineweight -1)) 'by-layer)
                 ((= lineweight -2) 'by-block)
                 ((= lineweight -3) 'default)
                 (T 'explicit)))
             (cons 'observed-index index))))

  (setq entities-from-refs
        '(lambda (refs / index item items reference state)
           (setq index 0)
           (while (and refs (null state))
             (setq reference (actl:dxf (car refs)))
             (cond
               ((null reference)
                (setq state 'absent))
               ((eq (car reference) 'error)
                (setq state reference))
               (T
                (setq item
                      (apply entity-item (list reference index)))
                (setq items (cons item items))
                (setq index (1+ index))
                (setq refs (cdr refs)))))

           (cond
             ((eq state 'absent) nil)
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
             (list (cdr (assoc 'items (cdr outcome))))))
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
   color-source
   extension-dictionary
   handle
   linetype-source
   lineweight-source
   outcome
   owner-handle
   reference
   transparency-source
   xdata-apps)
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
                       (assoc 5 data))
                  (strcase (cdr (assoc 5 data)))))))

      (setq owner-handle
            '(lambda (data / depth owner pair)
               (setq depth 0)
               (foreach pair data
                 (cond
                   ((and (= (car pair) 102)
                         (/= (cdr pair) "}"))
                    (setq depth (1+ depth)))
                   ((and (= (car pair) 102)
                         (= (cdr pair) "}"))
                    (setq depth (1- depth)))
                   ((and (= depth 0)
                         (= (car pair) 330)
                         (null owner))
                    (setq owner (cdr pair)))))
               owner))

      (setq color-source
            '(lambda (data / aci)
               (setq aci (cdr (assoc 62 data)))
               (cond
                 ((or (assoc 420 data) (assoc 430 data)) 'explicit)
                 ((or (null aci) (= aci 256)) 'by-layer)
                 ((= aci 0) 'by-block)
                 (T 'explicit))))

      (setq linetype-source
            '(lambda (data / value)
               (setq value (cdr (assoc 6 data)))
               (cond
                 ((or (null value) (= (strcase value) "BYLAYER"))
                  'by-layer)
                 ((= (strcase value) "BYBLOCK") 'by-block)
                 (T 'explicit))))

      (setq lineweight-source
            '(lambda (data / value)
               (setq value (cdr (assoc 370 data)))
               (cond
                 ((or (null value) (= value -1)) 'by-layer)
                 ((= value -2) 'by-block)
                 ((= value -3) 'default)
                 (T 'explicit))))

      (setq transparency-source
            '(lambda (data / method value)
               (setq value (cdr (assoc 440 data)))
               (if value
                 (setq method (lsh value -24)))
               (cond
                 ((or (null method) (= method 0)) 'by-layer)
                 ((= method 1) 'by-block)
                 (T 'explicit))))

      (setq extension-dictionary
            '(lambda (data / active found pair)
               (foreach pair data
                 (cond
                   ((and (= (car pair) 102)
                         (= (cdr pair) "{ACAD_XDICTIONARY"))
                    (setq active T))
                   ((and active (= (car pair) 360))
                    (setq found T))
                   ((and active
                         (= (car pair) 102)
                         (= (cdr pair) "}"))
                    (setq active nil))))
               found))

      (setq xdata-apps
            '(lambda (data / apps pair)
               (foreach pair data
                 (if (and (= (car pair) -3)
                          (eq (type (car (cadr pair))) 'STR))
                   (setq apps (cons (car (cadr pair)) apps))))
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
                   (cons 'type (cdr (assoc 0 data)))
                   (cons 'owner-handle (apply handle (list owner)))
                   (cons 'layout (cdr (assoc 410 data)))
                   (cons 'layer (cdr (assoc 8 data)))
                   (cons 'visible (not (eq (cdr (assoc 60 data)) 1)))
                   (cons 'color-source (apply color-source (list data)))
                   (cons 'aci (cdr (assoc 62 data)))
                   (cons 'true-color (cdr (assoc 420 data)))
                   (cons 'color-book (cdr (assoc 430 data)))
                   (cons
                     'linetype-source
                     (apply linetype-source (list data)))
                   (cons 'linetype (cdr (assoc 6 data)))
                   (cons
                     'lineweight-source
                     (apply lineweight-source (list data)))
                   (cons 'lineweight (cdr (assoc 370 data)))
                   (cons
                     'transparency-source
                     (apply transparency-source (list data)))
                   (cons 'transparency (cdr (assoc 440 data)))
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

(defun actl:xdata (subject applications / outcome reference)
  (if (not
        (or
          (eq applications 'all)
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
                       (if (= (car pair) -3)
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
            outcome))))))
