(defun actl:dict
  (path depth /
   collect-entries
   dictionary-p
   dxf-dictionary-key-code
   dxf-entity-name-code
   dxf-hard-owner-code
   dxf-soft-owner-code
   dxf-subclass-code
   dxf-type-code
   entry-item
   outcome
   read-object
   resolve-path
   root
   string-list-p
   visited
   walk-dictionary)
  (setq dxf-dictionary-key-code 3)
  (setq dxf-entity-name-code -1)
  (setq dxf-hard-owner-code 360)
  (setq dxf-soft-owner-code 350)
  (setq dxf-subclass-code 100)
  (setq dxf-type-code 0)

  (setq string-list-p
        '(lambda (value / valid)
           (setq valid T)
           (while (and valid value)
             (if (and (eq (type value) 'LIST)
                      (eq (type (car value)) 'STR))
               (setq value (cdr value))
               (setq valid nil)))
           valid))

  (cond
    ((not (apply string-list-p (list path)))
     (actl:err "Expected a list of dictionary keys"))
    ((not (and (eq (type depth) 'INT) (>= depth 0)))
     (actl:err "Expected a nonnegative integer depth"))
    (T
     (setq dictionary-p
           '(lambda (object-type)
              (or (= object-type "DICTIONARY")
                  (= object-type "ACDBDICTIONARYWDFLT"))))

     (setq read-object
           '(lambda (subject / reference)
              (setq reference (actl:dxf subject))
              (cond
                ((null reference)
                 (actl:err "A dictionary entry is unavailable"))
                ((eq (car reference) 'error) reference)
                (T reference))))

     (setq collect-entries
           '(lambda (data / active entries key keys malformed pair)
              (foreach pair data
                (cond
                  ((and (= (car pair) dxf-subclass-code)
                        (= (cdr pair) "AcDbDictionary"))
                   (setq active T))
                  ((and active
                        (= (car pair) dxf-dictionary-key-code))
                   (if (or key
                           (not (eq (type (cdr pair)) 'STR))
                           (member (cdr pair) keys))
                     (setq malformed T)
                     (progn
                       (setq key (cdr pair))
                       (setq keys (cons key keys)))))
                  ((and active
                        (or (= (car pair) dxf-soft-owner-code)
                            (= (car pair) dxf-hard-owner-code)))
                   (if key
                     (progn
                       (setq entries
                             (cons (cons key (cdr pair)) entries))
                       (setq key nil))
                     (setq malformed T)))))
              (if (or key (null active)) (setq malformed T))
              (if malformed
                (actl:err "Dictionary entries are malformed")
                (vl-sort
                  entries
                  '(lambda (left right)
                     (< (car left) (car right)))))))

     (setq walk-dictionary
           '(lambda (dictionary remaining / data entries handle item items reference)
              (setq reference (apply read-object (list dictionary)))
              (if (eq (car reference) 'error)
                reference
                (progn
                  (setq data (cdr (assoc 'value (cdr reference))))
                  (setq handle (cdr (assoc 'handle (cdr reference))))
                  (if (not
                        (apply
                          dictionary-p
                          (list (cdr (assoc dxf-type-code data)))))
                    (actl:err "The selected object is not a dictionary")
                    (if (member handle visited)
                      (actl:err
                        (strcat
                          "Dictionary traversal revisited handle "
                          handle))
                      (progn
                        (setq visited (cons handle visited))
                        (setq entries (apply collect-entries (list data)))
                        (if (and (eq (type entries) 'LIST)
                                 (eq (car entries) 'error))
                          entries
                          (progn
                            (while (and entries (null item))
                              (setq item
                                    (apply
                                      entry-item
                                      (list (car entries) remaining)))
                              (if (eq (car item) 'error)
                                (setq entries nil)
                                (progn
                                  (setq items (cons item items))
                                  (setq item nil)
                                  (setq entries (cdr entries)))))
                            (if item
                              item
                              (actl:ok
                                (list
                                  (cons 'handle handle)
                                  (cons 'entries (reverse items))))))))))))))

     (setq entry-item
           '(lambda (entry remaining / child data handle item object-type reference)
              (setq reference (apply read-object (list (cdr entry))))
              (if (eq (car reference) 'error)
                reference
                (progn
                  (setq data (cdr (assoc 'value (cdr reference))))
                  (setq handle (cdr (assoc 'handle (cdr reference))))
                  (setq object-type (cdr (assoc dxf-type-code data)))
                  (setq item
                        (list
                          (cons 'key (car entry))
                          (cons 'type object-type)
                          (cons 'handle handle)))
                  (cond
                    ((= object-type "XRECORD")
                     (append item (list (cons 'value data))))
                    ((and (> remaining 0)
                          (apply dictionary-p (list object-type)))
                     (setq child
                           (apply
                             walk-dictionary
                             (list (cdr entry) (1- remaining))))
                     (if (eq (car child) 'error)
                       child
                       (append
                         item
                         (list
                           (cons
                             'entries
                             (cdr (assoc 'entries (cdr child))))))))
                    (T item))))))

     (setq resolve-path
           '(lambda (keys / current data key reference state)
              (setq current (namedobjdict))
              (while (and keys (null state))
                (setq key (car keys))
                (setq data (dictsearch current key))
                (if (null data)
                  (setq state 'absent)
                  (progn
                    (setq current
                          (cdr (assoc dxf-entity-name-code data)))
                    (if (null current)
                      (setq state
                            (actl:err
                              "A dictionary path entry has no object reference"))
                      (progn
                        (setq reference
                              (apply read-object (list current)))
                        (if (eq (car reference) 'error)
                          (setq state reference)
                          (if (not
                                (apply
                                  dictionary-p
                                  (list
                                    (cdr
                                      (assoc
                                        dxf-type-code
                                        (cdr
                                          (assoc
                                            'value
                                            (cdr reference))))))))
                            (setq state
                                  (actl:err
                                    (strcat
                                      "Dictionary path entry is not a dictionary: "
                                      key)))))))))
                (setq keys (cdr keys)))
              (cond
                ((eq state 'absent) nil)
                (state state)
                (T current))))

     (setq outcome
           (vl-catch-all-apply
             '(lambda ()
                (setq root (apply resolve-path (list path)))
                (cond
                  ((null root) nil)
                  ((and (eq (type root) 'LIST)
                        (eq (car root) 'error))
                   root)
                  (T (apply walk-dictionary (list root depth)))))
             '()))
     (if (vl-catch-all-error-p outcome)
       (actl:err
         (strcat
           "Could not inspect the dictionary: "
           (vl-catch-all-error-message outcome)))
       outcome))))

(defun actl:extdict
  (subject depth /
   active
   collect-entries
   dictionary
   dictionary-p
   dxf-control-string-code
   dxf-dictionary-key-code
   dxf-hard-owner-code
   dxf-soft-owner-code
   dxf-subclass-code
   dxf-type-code
   entry-item
   malformed
   marker
   outcome
   pair
   read-object
   reference
   visited
   walk-dictionary)
  (setq dxf-control-string-code 102)
  (setq dxf-dictionary-key-code 3)
  (setq dxf-hard-owner-code 360)
  (setq dxf-soft-owner-code 350)
  (setq dxf-subclass-code 100)
  (setq dxf-type-code 0)

  (if (not (and (eq (type depth) 'INT) (>= depth 0)))
    (actl:err "Expected a nonnegative integer depth")
    (progn
      (setq reference (actl:dxf subject))
      (if (or (null reference) (eq (car reference) 'error))
        reference
        (progn
          (foreach pair (cdr (assoc 'value (cdr reference)))
            (cond
              ((and (= (car pair) dxf-control-string-code)
                    (= (cdr pair) "{ACAD_XDICTIONARY"))
               (if (or marker active)
                 (setq malformed T)
                 (progn
                   (setq marker T)
                   (setq active T))))
              ((and active (= (car pair) dxf-hard-owner-code))
               (if dictionary
                 (setq malformed T)
                 (setq dictionary (cdr pair))))
              ((and active
                    (= (car pair) dxf-control-string-code)
                    (= (cdr pair) "}"))
               (if (null dictionary) (setq malformed T))
               (setq active nil))))

          (cond
            ((or malformed active)
             (actl:err "The extension dictionary reference is malformed"))
            ((null marker) nil)
            (T
             (setq dictionary-p
                   '(lambda (object-type)
                      (or (= object-type "DICTIONARY")
                          (= object-type "ACDBDICTIONARYWDFLT"))))

             (setq read-object
                   '(lambda (object / result)
                      (setq result (actl:dxf object))
                      (cond
                        ((null result)
                         (actl:err
                           "A dictionary entry is unavailable"))
                        ((eq (car result) 'error) result)
                        (T result))))

             (setq collect-entries
                   '(lambda (data / active entries invalid item key keys)
                      (foreach item data
                        (cond
                          ((and (= (car item) dxf-subclass-code)
                                (= (cdr item) "AcDbDictionary"))
                           (setq active T))
                          ((and active
                                (= (car item) dxf-dictionary-key-code))
                           (if (or key
                                   (not (eq (type (cdr item)) 'STR))
                                   (member (cdr item) keys))
                             (setq invalid T)
                             (progn
                               (setq key (cdr item))
                               (setq keys (cons key keys)))))
                          ((and active
                                (or (= (car item) dxf-soft-owner-code)
                                    (= (car item) dxf-hard-owner-code)))
                           (if key
                             (progn
                               (setq entries
                                     (cons
                                       (cons key (cdr item))
                                       entries))
                               (setq key nil))
                             (setq invalid T)))))
                      (if (or key (null active)) (setq invalid T))
                      (if invalid
                        (actl:err "Dictionary entries are malformed")
                        (vl-sort
                          entries
                          '(lambda (left right)
                             (< (car left) (car right)))))))

             (setq walk-dictionary
                   '(lambda (object remaining / data entries handle item items result)
                      (setq result (apply read-object (list object)))
                      (if (eq (car result) 'error)
                        result
                        (progn
                          (setq data
                                (cdr (assoc 'value (cdr result))))
                          (setq handle
                                (cdr (assoc 'handle (cdr result))))
                          (if (not
                                (apply
                                  dictionary-p
                                  (list
                                    (cdr
                                      (assoc dxf-type-code data)))))
                            (actl:err
                              "The selected object is not a dictionary")
                            (if (member handle visited)
                              (actl:err
                                (strcat
                                  "Dictionary traversal revisited handle "
                                  handle))
                              (progn
                                (setq visited (cons handle visited))
                                (setq entries
                                      (apply collect-entries (list data)))
                                (if (and (eq (type entries) 'LIST)
                                         (eq (car entries) 'error))
                                  entries
                                  (progn
                                    (while (and entries (null item))
                                      (setq item
                                            (apply
                                              entry-item
                                              (list
                                                (car entries)
                                                remaining)))
                                      (if (eq (car item) 'error)
                                        (setq entries nil)
                                        (progn
                                          (setq items (cons item items))
                                          (setq item nil)
                                          (setq entries (cdr entries)))))
                                    (if item
                                      item
                                      (actl:ok
                                        (list
                                          (cons 'handle handle)
                                          (cons
                                            'entries
                                            (reverse items))))))))))))))

             (setq entry-item
                   '(lambda (entry remaining / child data handle item object-type result)
                      (setq result
                            (apply read-object (list (cdr entry))))
                      (if (eq (car result) 'error)
                        result
                        (progn
                          (setq data
                                (cdr (assoc 'value (cdr result))))
                          (setq handle
                                (cdr (assoc 'handle (cdr result))))
                          (setq object-type
                                (cdr (assoc dxf-type-code data)))
                          (setq item
                                (list
                                  (cons 'key (car entry))
                                  (cons 'type object-type)
                                  (cons 'handle handle)))
                          (cond
                            ((= object-type "XRECORD")
                             (append
                               item
                               (list (cons 'value data))))
                            ((and (> remaining 0)
                                  (apply
                                    dictionary-p
                                    (list object-type)))
                             (setq child
                                   (apply
                                     walk-dictionary
                                     (list
                                       (cdr entry)
                                       (1- remaining))))
                             (if (eq (car child) 'error)
                               child
                               (append
                                 item
                                 (list
                                   (cons
                                     'entries
                                     (cdr
                                       (assoc
                                         'entries
                                         (cdr child))))))))
                            (T item))))))

             (setq outcome
                   (vl-catch-all-apply
                     walk-dictionary
                     (list dictionary depth)))
             (if (vl-catch-all-error-p outcome)
               (actl:err
                 (strcat
                   "Could not inspect the extension dictionary: "
                   (vl-catch-all-error-message outcome)))
               outcome))))))))
