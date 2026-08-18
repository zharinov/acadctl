(defun actl:print
  (value /
   continue
   emit-atom
   emit-event
   emit-item
   emit-tail
   emit-task
   emit-text
   first-character-position
   max-value-depth
   output-event-codes
   stack
   state
   task
   text-chunk-size)
  (setq output-event-codes
        '((begin-list . 1)
          (end-list . 2)
          (dot . 3)
          (nil . 4)
          (true . 5)
          (integer . 6)
          (real . 7)
          (begin-string . 8)
          (string-chunk . 9)
          (end-string . 10)
          (begin-symbol . 11)
          (symbol-chunk . 12)
          (end-symbol . 13)
          (entity . 14)
          (selection-set . 15)
          (vla-object . 16)
          (file . 17)
          (function . 18)
          (error-object . 19)
          (object . 20)
          (cycle . 21)
          (too-deep . 22)
          (begin-value . 23)
          (end-value . 24)))
  (setq first-character-position 1)
  (setq text-chunk-size 2048)
  (setq max-value-depth 4096)

  (setq emit-event
        '(lambda (event value / code)
           (setq code (cdr (assoc event output-event-codes)))

           (if code
             (actl:_output-event code value)
             (actl:_invalid-output-event))))

  (setq emit-text
        '(lambda
           (text begin-event chunk-event end-event /
            continue
            offset
            part)
           (setq continue
                 (apply emit-event (list begin-event nil)))
           (if continue
             (progn
               (setq offset first-character-position)
               (setq part (substr text offset text-chunk-size))

               (while (and continue (/= part ""))
                 (setq continue
                       (apply emit-event (list chunk-event part)))
                 (setq offset (+ offset text-chunk-size))
                 (if continue
                   (setq part
                         (substr text offset text-chunk-size))))

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
        '(lambda (task stack / continue depth value)
           (setq value (cadr task))
           (setq depth (caddr task))

           (if (vl-consp value)
             (if (>= depth max-value-depth)
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

(defun actl:println (text / emit-event output-event-codes)
  (setq output-event-codes
        '((println . 25)
          (invalid-println . 26)))
  (setq emit-event
        '(lambda (event value / code)
           (setq code (cdr (assoc event output-event-codes)))
           (if code
             (actl:_output-event code value)
             (actl:_invalid-output-event))))

  (if (eq (type text) 'STR)
    (apply emit-event (list 'println text))
    (apply emit-event (list 'invalid-println nil)))
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
