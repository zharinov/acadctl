(defun actl:_read-ref (subject / ename data)
  (setq ename
        (if (eq (type subject) 'STR)
          (handent subject)
          subject))
  (if ename
    (setq data (entget ename)))
  (list ename data))

(defun actl:_ref (subject / outcome ename data handle)
  (if (not (or (eq (type subject) 'STR)
               (eq (type subject) 'ENAME)))
    (actl:_err
      'invalid-subject
      subject
      "Expected a handle string or entity name")
    (progn
      (setq outcome
            (vl-catch-all-apply
              'actl:_read-ref
              (list subject)))
      (if (vl-catch-all-error-p outcome)
        (actl:_err
          'read-failed
          subject
          (vl-catch-all-error-message outcome))
        (progn
          (setq ename (car outcome))
          (setq data (cadr outcome))
          (cond
            ((null data)
             nil)
            ((null (setq handle (cdr (assoc 5 data))))
             (actl:_err
               'missing-handle
               subject
               "The object has no DXF handle"))
            (T
              (actl:_ok
                (list
                  (cons 'handle (strcase handle))
                  (cons 'ename ename)
                  (cons 'dxf data))))))))))
