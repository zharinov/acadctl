(defun actl:print
  (value /
   continue
   emit-atom
   emit-event
   emit-item
   emit-tail
   emit-task
   emit-text
   stack
   state
   task)
  (setq emit-event
        '(lambda (event value / code)
           (setq code
                 (cond
                   ((eq event 'begin-list) 1)
                   ((eq event 'end-list) 2)
                   ((eq event 'dot) 3)
                   ((eq event 'nil) 4)
                   ((eq event 'true) 5)
                   ((eq event 'integer) 6)
                   ((eq event 'real) 7)
                   ((eq event 'begin-string) 8)
                   ((eq event 'string-chunk) 9)
                   ((eq event 'end-string) 10)
                   ((eq event 'begin-symbol) 11)
                   ((eq event 'symbol-chunk) 12)
                   ((eq event 'end-symbol) 13)
                   ((eq event 'entity) 14)
                   ((eq event 'selection-set) 15)
                   ((eq event 'vla-object) 16)
                   ((eq event 'file) 17)
                   ((eq event 'function) 18)
                   ((eq event 'error-object) 19)
                   ((eq event 'object) 20)
                   ((eq event 'cycle) 21)
                   ((eq event 'too-deep) 22)
                   ((eq event 'begin-value) 23)
                   ((eq event 'end-value) 24)))

           (if code
             (actl:_output-event code value)
             (actl:_invalid-output-event))))

  (setq emit-text
        '(lambda
           (text begin-event chunk-event end-event /
            chunk-size
            continue
            offset
            part)
           (setq chunk-size 2048)
           (setq continue
                 (apply emit-event (list begin-event nil)))
           (if continue
             (progn
               (setq offset 1)
               (setq part (substr text offset chunk-size))

               (while (and continue (/= part ""))
                 (setq continue
                       (apply emit-event (list chunk-event part)))
                 (setq offset (+ offset chunk-size))
                 (if continue
                   (setq part
                         (substr text offset chunk-size))))

               (if continue
                 (setq continue
                       (apply emit-event (list end-event nil))))))

           continue))

  (setq emit-atom
        '(lambda (value / value-type)
           (cond
             ((null value)
              (apply emit-event (list 'nil nil)))
             ((eq value T)
              (apply emit-event (list 'true nil)))
             ((vl-catch-all-error-p value)
              (apply emit-event (list 'error-object nil)))
             (T
              (setq value-type (type value))
              (cond
                ((eq value-type 'INT)
                 (apply emit-event (list 'integer value)))
                ((eq value-type 'REAL)
                 (apply emit-event (list 'real value)))
                ((eq value-type 'STR)
                 (apply
                   emit-text
                   (list
                     value
                     'begin-string
                     'string-chunk
                     'end-string)))
                ((eq value-type 'SYM)
                 (apply
                   emit-text
                   (list
                     (vl-symbol-name value)
                     'begin-symbol
                     'symbol-chunk
                     'end-symbol)))
                ((eq value-type 'ENAME)
                 (apply emit-event (list 'entity value)))
                ((eq value-type 'PICKSET)
                 (apply emit-event (list 'selection-set nil)))
                ((eq value-type 'VLA-OBJECT)
                 (apply emit-event (list 'vla-object nil)))
                ((eq value-type 'FILE)
                 (apply emit-event (list 'file nil)))
                ((or (eq value-type 'SUBR)
                     (eq value-type 'USUBR)
                     (eq value-type 'EXRXSUBR))
                 (apply emit-event (list 'function nil)))
                (T
                 (apply
                   emit-event
                   (list 'object (vl-symbol-name value-type)))))))))

  (setq emit-item
        '(lambda (task stack / continue depth max-depth value)
           (setq value (cadr task))
           (setq depth (caddr task))
           (setq max-depth 4096)

           (if (vl-consp value)
             (if (>= depth max-depth)
               (list
                 (apply emit-event (list 'too-deep nil))
                 stack)
               (progn
                 (setq continue
                       (apply emit-event (list 'begin-list nil)))
                 (if continue
                   (list
                     continue
                     (cons
                       (list 'tail value value value depth)
                       stack))
                   (list continue stack))))
             (list (apply emit-atom (list value)) stack))))

  (setq emit-tail
        '(lambda
           (task stack /
            continue
            depth
            fast
            next-fast
            next-slow
            slow
            tail)
           (setq tail (cadr task))
           (setq slow (caddr task))
           (setq fast (cadddr task))
           (setq depth (car (cddddr task)))

           (cond
             ((null tail)
              (list
                (apply emit-event (list 'end-list nil))
                stack))
             ((vl-consp tail)
              (setq next-slow
                    (if (vl-consp slow)
                      (cdr slow)
                      nil))
              (setq next-fast
                    (if (and (vl-consp fast)
                             (vl-consp (cdr fast)))
                      (cdr (cdr fast))
                      nil))

              (if (and (vl-consp next-slow)
                       (eq next-slow next-fast))
                (list
                  T
                  (cons
                    (list 'item (car tail) (+ depth 1))
                    (cons
                      '(dot)
                      (cons '(cycle)
                            (cons '(end-list) stack)))))
                (list
                  T
                  (cons
                    (list 'item (car tail) (+ depth 1))
                    (cons
                      (list
                        'tail
                        (cdr tail)
                        next-slow
                        next-fast
                        depth)
                      stack)))))
             (T
              (setq continue
                    (apply emit-event (list 'dot nil)))
              (if continue
                (list
                  continue
                  (cons
                    (list 'item tail (+ depth 1))
                    (cons '(end-list) stack)))
                (list continue stack))))))

  (setq emit-task
        '(lambda (task stack / kind)
           (setq kind (car task))
           (cond
             ((eq kind 'item)
              (apply emit-item (list task stack)))
             ((eq kind 'tail)
              (apply emit-tail (list task stack)))
             ((eq kind 'end-list)
              (list
                (apply emit-event (list 'end-list nil))
                stack))
             ((eq kind 'dot)
              (list
                (apply emit-event (list 'dot nil))
                stack))
             ((eq kind 'cycle)
              (list
                (apply emit-event (list 'cycle nil))
                stack))
             (T
              (actl:_invalid-value-task)))))

  (setq continue
        (apply emit-event (list 'begin-value nil)))
  (setq stack
        (if continue
          (list (list 'item value 0))))

  (while (and continue stack)
    (setq task (car stack))
    (setq stack (cdr stack))
    (setq state (apply emit-task (list task stack)))
    (setq continue (car state))
    (setq stack (cadr state)))

  (apply emit-event (list 'end-value nil))
  value)

(defun actl:label (text / emit-event)
  (setq emit-event
        '(lambda (event value / code)
           (setq code
                 (cond
                   ((eq event 'label) 25)
                   ((eq event 'invalid-label) 26)))
           (if code
             (actl:_output-event code value)
             (actl:_invalid-output-event))))

  (if (eq (type text) 'STR)
    (apply emit-event (list 'label text))
    (apply emit-event (list 'invalid-label nil)))
  nil)

(defun actl:_emit-retained-value (/ outcome value)
  (setq value actl:*bridge-value*)
  (setq actl:*bridge-value* nil)
  (setq outcome
        (vl-catch-all-apply
          '(lambda () (actl:print value))
          '()))
  (setq actl:*bridge-errno* (getvar "ERRNO"))

  (if (vl-catch-all-error-p outcome)
    (progn
      (setq actl:*bridge-status* nil)
      (setq actl:*bridge-error*
            (vl-catch-all-error-message outcome)))
    (progn
      (setq actl:*bridge-status* T)
      (setq actl:*bridge-error* nil)))

  (princ))
