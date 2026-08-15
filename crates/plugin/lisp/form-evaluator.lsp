(progn
  ((lambda (/ acadctl:forms acadctl:outcome)
     (setq acadctl:outcome
       (vl-catch-all-apply
         '(lambda ()
            (setq acadctl:forms
              (read (strcat "(" acadctl:*bridge-source* "\n)")))
            (if (= (length acadctl:forms) 1)
              (list 'acadctl:ok (eval (car acadctl:forms)))
              (acadctl:_invalid-form-span)))
         '()))

     (setq acadctl:*bridge-errno* (getvar "ERRNO"))

     (if (vl-catch-all-error-p acadctl:outcome)
       (progn
         (setq acadctl:*bridge-status* nil)
         (setq acadctl:*bridge-error*
           (vl-catch-all-error-message acadctl:outcome)))
       (progn
         (setq acadctl:*bridge-value* (cadr acadctl:outcome))
         (setq acadctl:*bridge-status* T)
         (setq acadctl:*bridge-error* nil)))))

  (setq acadctl:*bridge-source* nil)
  (princ))
