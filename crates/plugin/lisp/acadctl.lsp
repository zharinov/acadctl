(progn
  ((lambda (/ acadctl:forms acadctl:outcome)
     (setq acadctl:outcome
       (vl-catch-all-apply
         '(lambda ()
            (setq acadctl:forms
              (read (strcat "(" acadctl:*source* "\n)")))
            (if (= (length acadctl:forms) 1)
              (list 'acadctl:ok (eval (car acadctl:forms)))
              (acadctl:invalid-form-span)))
         '()))
     (setq acadctl:*errno* (getvar "ERRNO"))
     (if (vl-catch-all-error-p acadctl:outcome)
       (progn
         (setq acadctl:*status* nil)
         (setq acadctl:*error*
           (vl-catch-all-error-message acadctl:outcome)))
       (progn
         (setq acadctl:*value* (cadr acadctl:outcome))
         (setq acadctl:*status* T)
         (setq acadctl:*error* nil)))))
  (setq acadctl:*source* nil)
  (princ))
