(defun actl:ok (value)
  (cons 'ok value))

(defun actl:err (message)
  (list 'error message))
