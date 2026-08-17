(defun actl:_emit-event (event value / code)
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
          ((eq event 'end-value) 24)
          ((eq event 'label) 25)
          ((eq event 'invalid-label) 26)))

  (if code
    (actl:_output-event code value)
    (actl:_invalid-output-event)))

(defun actl:_emit-text
  (text begin-event chunk-event end-event /
   continue
   offset
   part
   chunk-size)
  (setq chunk-size 2048)
  (setq continue
        (actl:_emit-event begin-event nil))
  (if continue
    (progn
      (setq offset 1)
      (setq part
            (substr text offset chunk-size))

      (while (and continue (/= part ""))
        (setq continue
              (actl:_emit-event chunk-event part))
        (setq offset
              (+ offset chunk-size))
        (if continue
          (setq part
                (substr text
                        offset
                        chunk-size))))

      (if continue
        (setq continue
              (actl:_emit-event end-event nil)))))

  continue)

(defun actl:_emit-atom (value / value-type)
  (cond
    ((null value)
     (actl:_emit-event 'nil nil))
    ((eq value T)
     (actl:_emit-event 'true nil))
    ((vl-catch-all-error-p value)
     (actl:_emit-event 'error-object nil))
    (T
      (setq value-type (type value))
      (cond
        ((eq value-type 'INT)
         (actl:_emit-event 'integer value))
        ((eq value-type 'REAL)
         (actl:_emit-event 'real value))
        ((eq value-type 'STR)
         (actl:_emit-text value
                          'begin-string
                          'string-chunk
                          'end-string))
        ((eq value-type 'SYM)
         (actl:_emit-text (vl-symbol-name value)
                          'begin-symbol
                          'symbol-chunk
                          'end-symbol))
        ((eq value-type 'ENAME)
         (actl:_emit-event 'entity value))
        ((eq value-type 'PICKSET)
         (actl:_emit-event 'selection-set nil))
        ((eq value-type 'VLA-OBJECT)
         (actl:_emit-event 'vla-object nil))
        ((eq value-type 'FILE)
         (actl:_emit-event 'file nil))
        ((or (eq value-type 'SUBR)
             (eq value-type 'USUBR)
             (eq value-type 'EXRXSUBR))
         (actl:_emit-event 'function nil))
        (T
          (actl:_emit-event
            'object
            (vl-symbol-name value-type)))))))

(defun actl:_item-task (value depth)
  (list 'item value depth))

(defun actl:_tail-task
  (tail slow fast depth)
  (list 'tail tail slow fast depth))

(defun actl:_emit-item
  (task stack /
   value
   depth
   continue
   max-depth)
  (setq value (cadr task))
  (setq depth (caddr task))
  (setq max-depth 4096)

  (if (vl-consp value)
    (if (>= depth max-depth)
      (list
        (actl:_emit-event 'too-deep nil)
        stack)
      (progn
        (setq continue
              (actl:_emit-event 'begin-list nil))
        (if continue
          (list
            continue
            (cons
              (actl:_tail-task
                value
                value
                value
                depth)
              stack))
          (list continue stack))))
    (list (actl:_emit-atom value) stack)))

(defun actl:_emit-tail
  (task stack /
   tail
   slow
   fast
   depth
   next-slow
   next-fast
   continue)
  (setq tail (cadr task))
  (setq slow (caddr task))
  (setq fast (cadddr task))
  (setq depth (car (cddddr task)))

  (cond
    ((null tail)
     (list
       (actl:_emit-event 'end-list nil)
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
           (actl:_item-task (car tail) (+ depth 1))
           (cons '(dot)
                 (cons '(cycle)
                       (cons '(end-list) stack)))))
       (list
         T
         (cons
           (actl:_item-task (car tail) (+ depth 1))
           (cons
             (actl:_tail-task
               (cdr tail)
               next-slow
               next-fast
               depth)
             stack)))))
    (T
      (setq continue
            (actl:_emit-event 'dot nil))
      (if continue
        (list
          continue
          (cons
            (actl:_item-task tail (+ depth 1))
            (cons '(end-list) stack)))
        (list continue stack)))))

(defun actl:_emit-task (task stack / kind)
  (setq kind (car task))
  (cond
    ((eq kind 'item)
     (actl:_emit-item task stack))
    ((eq kind 'tail)
     (actl:_emit-tail task stack))
    ((eq kind 'end-list)
     (list
       (actl:_emit-event 'end-list nil)
       stack))
    ((eq kind 'dot)
     (list
       (actl:_emit-event 'dot nil)
       stack))
    ((eq kind 'cycle)
     (list
       (actl:_emit-event 'cycle nil)
       stack))
    (T
      (actl:_invalid-value-task))))

(defun actl:_emit-value
  (value /
   continue
   stack
   task
   state)
  (setq continue
        (actl:_emit-event 'begin-value nil))
  (setq stack
        (if continue
          (list (actl:_item-task value 0))))

  (while (and continue stack)
    (setq task (car stack))
    (setq stack (cdr stack))
    (setq state (actl:_emit-task task stack))
    (setq continue (car state))
    (setq stack (cadr state)))

  (actl:_emit-event 'end-value nil)
  nil)

(defun actl:print (value)
  (actl:_emit-value value)
  value)

(defun actl:label (text)
  (if (eq (type text) 'STR)
    (actl:_emit-event 'label text)
    (actl:_emit-event 'invalid-label nil))
  nil)

(defun actl:_emit-retained-value (/ value outcome)
  (setq value actl:*bridge-value*)
  (setq actl:*bridge-value* nil)
  (setq outcome
        (vl-catch-all-apply
          '(lambda () (actl:_emit-value value))
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
