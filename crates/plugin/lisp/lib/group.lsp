(defun actl:groups
  (/ collect dxf-dictionary-key-code dxf-hard-pointer-code dxf-handle-code dxf-soft-owner-code dxf-type-code group-record handle outcome)
  (setq dxf-dictionary-key-code 3)
  (setq dxf-hard-pointer-code 340)
  (setq dxf-handle-code 5)
  (setq dxf-soft-owner-code 350)
  (setq dxf-type-code 0)

  (setq handle
        '(lambda (entity / data value)
           (if (and entity
                    (setq data (entget entity))
                    (setq value (cdr (assoc dxf-handle-code data))))
             (strcase value))))

  (setq group-record
        '(lambda (name entity / data member members value)
           (if (null (setq data (entget entity)))
             (actl:err
               (list
                 (strcat "Could not read group " name)))
             (if (/= (cdr (assoc dxf-type-code data)) "GROUP")
               (actl:err
                 (list
                   (strcat
                     "Dictionary entry is not a group: "
                     name)))
               (progn
               (foreach value data
                 (if (= (car value) dxf-hard-pointer-code)
                   (if (null
                         (setq member
                               (apply handle (list (cdr value)))))
                     (setq members 'error)
                     (if (not (eq members 'error))
                       (setq members (cons member members))))))
               (if (eq members 'error)
                 (actl:err
                   (list
                     (strcat
                       "Could not resolve a member of group "
                       name)))
                 (actl:ok
                   (list
                     (cons 'name name)
                     (cons
                       'handle
                       (strcase (cdr (assoc dxf-handle-code data))))
                     (cons 'members (reverse members))))))))))

  (setq collect
        '(lambda (/ data entry group groups items key pair result)
           (if (null
                 (setq groups
                       (dictsearch (namedobjdict) "ACAD_GROUP")))
             (actl:ok (list (cons 'items nil)))
             (progn
               (foreach pair groups
                 (cond
                   ((= (car pair) dxf-dictionary-key-code)
                    (setq key (cdr pair)))
                   ((= (car pair) dxf-soft-owner-code)
                    (if key
                      (setq items
                            (cons
                              (list key (cdr pair))
                              items)))
                    (setq key nil))))
               (setq items
                     (vl-sort
                       items
                       '(lambda (left right)
                          (< (car left) (car right)))))
               (while (and items (null result))
                 (setq entry (car items))
                 (setq group
                       (apply
                         group-record
                         (list (car entry) (cadr entry))))
                 (if (eq (car group) 'error)
                   (setq result group)
                   (setq data (cons (cdr group) data)))
                 (setq items (cdr items)))
               (if result
                 result
                 (actl:ok
                   (list
                     (cons 'items (reverse data)))))))))

  (setq outcome (vl-catch-all-apply collect '()))
  (if (vl-catch-all-error-p outcome)
    (actl:err
      (list
        (strcat
          "Could not inspect groups: "
          (vl-catch-all-error-message outcome))))
    outcome))
