(progn
  ((lambda (/ acadctl:forms acadctl:outcome)
     (setq acadctl:outcome
       (vl-catch-all-apply
         '(lambda ()
            (setq acadctl:forms
              (read (strcat "(" {{SOURCE_SYMBOL}} "\n)")))
            (if (= (length acadctl:forms) 1)
              (list 'acadctl:ok (eval (car acadctl:forms)))
              ({{INVALID_FORM_SPAN_FUNCTION}})))
         '()))

     (setq {{ERRNO_SYMBOL}} (getvar "ERRNO"))

     (if (vl-catch-all-error-p acadctl:outcome)
       (progn
         (setq {{STATUS_SYMBOL}} nil)
         (setq {{ERROR_SYMBOL}}
           (vl-catch-all-error-message acadctl:outcome)))
       (progn
         (setq {{VALUE_SYMBOL}} (cadr acadctl:outcome))
         (setq {{STATUS_SYMBOL}} T)
         (setq {{ERROR_SYMBOL}} nil)))))

  (setq {{SOURCE_SYMBOL}} nil)
  (princ))
