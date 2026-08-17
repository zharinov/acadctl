(progn
  ((lambda (/ actl:forms actl:outcome)
     (setq actl:outcome
       (vl-catch-all-apply
         '(lambda ()
            (setq actl:forms
              (read (strcat "(" {{SOURCE_SYMBOL}} "\n)")))
            (if (= (length actl:forms) 1)
              (list 'actl:ok (eval (car actl:forms)))
              ({{INVALID_FORM_SPAN_FUNCTION}})))
         '()))

     (setq {{ERRNO_SYMBOL}} (getvar "ERRNO"))

     (if (vl-catch-all-error-p actl:outcome)
       (progn
         (setq {{STATUS_SYMBOL}} nil)
         (setq {{ERROR_SYMBOL}}
           (vl-catch-all-error-message actl:outcome)))
       (progn
         (setq {{VALUE_SYMBOL}} (cadr actl:outcome))
         (setq {{STATUS_SYMBOL}} T)
         (setq {{ERROR_SYMBOL}} nil)))))

  (setq {{SOURCE_SYMBOL}} nil)
  (princ))
