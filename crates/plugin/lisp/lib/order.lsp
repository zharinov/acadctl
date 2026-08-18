(defun actl:order
  (scope /
   add-order-index
   canonical-layout
   decode-mapping
   dxf-block-record-code
   dxf-entity-sort-code
   dxf-handle-code
   dxf-subclass-code
   dxf-type-code
   entities
   entity-result
   explicit-order
   find-entry
   hex-less
   layout-name
   make-result
   mapping
   outcome
   resolve-layout
   scope-record
   table-reference)
  (setq dxf-block-record-code 330)
  (setq dxf-entity-sort-code 331)
  (setq dxf-handle-code 5)
  (setq dxf-subclass-code 100)
  (setq dxf-type-code 0)

  (setq canonical-layout
        '(lambda (requested / candidate found)
           (if (= (strcase requested) "MODEL")
             (setq found "Model")
             (foreach candidate (layoutlist)
               (if (= (strcase candidate) (strcase requested))
                 (setq found candidate))))
           found))

  (setq hex-less
        '(lambda (left right / a b)
           (setq a (vl-string-left-trim "0" left))
           (setq b (vl-string-left-trim "0" right))
           (if (= a "") (setq a "0"))
           (if (= b "") (setq b "0"))
           (cond
             ((< (strlen a) (strlen b)) T)
             ((> (strlen a) (strlen b)) nil)
             (T (< a b)))))

  (setq add-order-index
        '(lambda (items / index item output)
           (setq index 0)
           (foreach item items
             (setq output
                   (cons
                     (append
                       item
                       (list (cons 'order-index index)))
                     output))
             (setq index (1+ index)))
           (reverse output)))

  (setq resolve-layout
        '(lambda (name / active block data dictionary pair)
           (if (and
                 (setq dictionary
                       (dictsearch (namedobjdict) "ACAD_LAYOUT"))
                 (setq data
                       (dictsearch
                         (cdr (assoc -1 dictionary))
                         name)))
             (progn
               (foreach pair data
                 (cond
                   ((and (= (car pair) dxf-subclass-code)
                         (= (cdr pair) "AcDbLayout"))
                    (setq active T))
                   ((and active
                         (= (car pair) dxf-block-record-code))
                    (setq block (cdr pair)))))
               block))))

  (setq find-entry
        '(lambda (result key / entry found)
           (foreach entry (cdr (assoc 'entries (cdr result)))
             (if (= (cdr (assoc 'key entry)) key)
               (setq found entry)))
           found))

  (setq decode-mapping
        '(lambda (reference / active data entity entity-handle index item items malformed pair seen-entities seen-sort sort-handle)
           (setq data (cdr (assoc 'value (cdr reference))))
           (setq index 0)
           (foreach pair data
             (cond
               ((and (= (car pair) dxf-subclass-code)
                     (= (cdr pair) "AcDbSortentsTable"))
                (setq active T))
               ((and active (= (car pair) dxf-entity-sort-code))
                (if entity
                  (setq malformed T)
                  (setq entity (cdr pair))))
               ((and active (= (car pair) dxf-handle-code) entity)
                (setq entity-handle nil)
                (setq item (actl:dxf entity))
                (cond
                  ((or (not (eq (type (cdr pair)) 'STR))
                       (null item)
                       (eq (car item) 'error))
                   (setq malformed T))
                  (T
                   (setq entity-handle
                         (cdr (assoc 'handle (cdr item))))
                   (setq sort-handle (strcase (cdr pair)))
                   (if (or (assoc entity-handle seen-entities)
                           (assoc sort-handle seen-sort))
                     (setq malformed T)
                     (progn
                       (setq seen-entities
                             (cons
                               (cons entity-handle T)
                               seen-entities))
                       (setq seen-sort
                             (cons
                               (cons sort-handle T)
                               seen-sort))
                       (setq items
                             (cons
                               (list
                                 (cons 'entity-handle entity-handle)
                                 (cons 'sort-handle sort-handle)
                                 (cons 'observed-index index))
                               items))
                       (setq index (1+ index))))))
                (setq entity nil))
               ((and active (= (car pair) dxf-handle-code))
                (setq malformed T))))
           (if (or malformed entity (null active))
             (actl:err
               "The stored draw-order mapping is malformed")
             (actl:ok
               (list
                 (cons 'items (reverse items)))))))

  (setq explicit-order
        '(lambda (items stored / effective entry handle item item-index mapped output row seen sort-handle)
           (foreach item items
             (setq item-index
                   (cons
                     (cons (cdr (assoc 'handle item)) item)
                     item-index)))
           (foreach entry stored
             (setq mapped
                   (cons
                     (cons
                       (cdr (assoc 'entity-handle entry))
                       entry)
                     mapped)))
           (foreach entry stored
             (if (null
                   (assoc
                     (cdr (assoc 'entity-handle entry))
                     item-index))
               (setq output
                     (actl:err
                       (strcat
                         "Draw-order entry is outside the requested scope: "
                         (cdr (assoc 'entity-handle entry)))))))
           (if (and output (eq (car output) 'error))
             output
             (progn
               (foreach item items
                 (setq handle (cdr (assoc 'handle item)))
                 (setq entry
                       (assoc
                         handle
                         mapped))
                 (setq sort-handle
                       (if entry
                         (cdr (assoc 'sort-handle (cdr entry)))
                         handle))
                 (if (assoc sort-handle seen)
                   (setq output
                         (actl:err
                           (strcat
                             "Draw-order sort handle is ambiguous: "
                             sort-handle)))
                   (progn
                     (setq seen (cons (cons sort-handle T) seen))
                     (setq effective
                           (cons
                             (cons sort-handle item)
                             effective)))))
               (if (and output (eq (car output) 'error))
                 output
                 (progn
                   (setq effective
                         (vl-sort
                           effective
                           '(lambda (left right)
                              (apply
                                hex-less
                                (list (car left) (car right))))))
                   (foreach row effective
                     (setq output (cons (cdr row) output)))
                   (actl:ok
                     (list
                       (cons
                         'items
                         (apply
                           add-order-index
                           (list (reverse output))))))))))))

  (setq make-result
        '(lambda (kind items stored)
           (actl:ok
             (list
               (list
                 'scope
                 (cons
                   'kind
                   (if (= layout-name "Model") 'model 'layout))
                 (cons 'name layout-name))
               (cons 'order kind)
               (cons 'direction 'back-to-front)
               (cons 'mapping stored)
               (cons 'items items)))))

  (setq outcome
        (vl-catch-all-apply
          '(lambda (/ block block-handle block-reference extdict indexed order-result stored table)
             (cond
               ((eq scope 'model)
                (setq layout-name "Model"))
               ((and (eq (type scope) 'LIST)
                     (eq (car scope) 'layout)
                     (eq (type (cdr scope)) 'STR))
                (setq layout-name
                      (apply canonical-layout (list (cdr scope)))))
               (T
                (setq order-result
                      (actl:err
                        "Expected model or a layout"))))
             (if (and (null order-result) (null layout-name))
               (setq order-result 'absent))
             (if (and (null order-result) layout-name)
               (progn
                 (setq entity-result
                       (actl:entities
                         (if (= layout-name "Model")
                           'model
                           (cons 'layout layout-name))))
                 (cond
                   ((null entity-result)
                    (setq order-result nil))
                   ((eq (car entity-result) 'error)
                    (setq order-result entity-result))
                   (T
                    (setq entities
                          (cdr
                            (assoc
                              'items
                              (cdr entity-result))))))))
             (if (null order-result)
               (progn
                 (setq block
                       (apply resolve-layout (list layout-name)))
                 (if (null block)
                   (setq order-result
                         (actl:err
                           "Layout ownership data is inconsistent")))))
             (if (null order-result)
               (progn
                 (setq block-reference (actl:dxf block))
                 (cond
                   ((null block-reference)
                    (setq order-result
                          (actl:err
                            "The layout block record is unavailable")))
                   ((eq (car block-reference) 'error)
                    (setq order-result block-reference))
                   (T
                    (setq block-handle
                          (cdr
                            (assoc
                              'handle
                              (cdr block-reference))))
                    (setq entities
                          (vl-remove-if-not
                            '(lambda (item)
                               (=
                                 (cdr (assoc 'owner-handle item))
                                 block-handle))
                            entities))))))
             (if (null order-result)
               (progn
                 (setq extdict (actl:extdict block 0))
                 (if (and extdict (eq (car extdict) 'error))
                   (setq order-result extdict))))
             (if (null order-result)
               (progn
                 (if extdict
                   (setq table
                         (apply
                           find-entry
                           (list extdict "ACAD_SORTENTS"))))
                 (cond
                   ((null table)
                    (setq indexed
                          (apply add-order-index (list entities)))
                    (setq order-result
                          (apply
                            make-result
                            (list 'default indexed nil))))
                   ((/= (cdr (assoc 'type table)) "SORTENTSTABLE")
                    (setq order-result
                          (actl:err
                            "ACAD_SORTENTS is not a SORTENTSTABLE"))))))
             (if (null order-result)
               (progn
                 (setq table-reference
                       (actl:dxf (cdr (assoc 'handle table))))
                 (cond
                   ((null table-reference)
                    (setq order-result
                          (actl:err
                            "The draw-order table is unavailable")))
                   ((eq (car table-reference) 'error)
                    (setq order-result table-reference)))))
             (if (null order-result)
               (progn
                 (setq mapping
                       (apply
                         decode-mapping
                         (list table-reference)))
                 (if (eq (car mapping) 'error)
                   (setq order-result mapping)
                   (setq stored
                         (cdr
                           (assoc
                             'items
                             (cdr mapping)))))))
             (if (null order-result)
               (progn
                 (setq indexed
                       (apply explicit-order (list entities stored)))
                 (if (eq (car indexed) 'error)
                   (setq order-result indexed)
                   (setq order-result
                         (apply
                           make-result
                           (list
                             'explicit
                             (cdr
                               (assoc
                                 'items
                                 (cdr indexed)))
                             stored))))))
             (if (eq order-result 'absent)
               nil
               order-result))
          '()))
  (if (vl-catch-all-error-p outcome)
    (actl:err
      (strcat
        "Could not inspect draw order: "
        (vl-catch-all-error-message outcome)))
    outcome))
