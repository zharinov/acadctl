(defun actl:_ok (fields)
  (cons 'ok fields))

(defun actl:_err (code subject message)
  (list
    'error
    (cons 'code code)
    (cons 'subject subject)
    (cons 'message message)))
