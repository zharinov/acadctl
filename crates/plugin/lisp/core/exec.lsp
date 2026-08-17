(defun actl:_drive-execution (/ actl:staged-form)
  (while (actl:_advance-execution)
    (setq actl:staged-form (read actl:*bridge-staged-form*))
    (if (eq actl:staged-form 'actl:_eval)
      ((lambda (/ actl:forms actl:outcome)
         (setq actl:outcome
               (vl-catch-all-apply
                 '(lambda ()
                    (setq actl:forms
                          (read (strcat "(" actl:*bridge-source* "\n)")))
                    (if (= (length actl:forms) 1)
                      (list 'actl:ok (eval (car actl:forms)))
                      (actl:_invalid-form-span)))
                 '()))

         (setq actl:*bridge-errno* (getvar "ERRNO"))

         (if (vl-catch-all-error-p actl:outcome)
           (progn
             (setq actl:*bridge-status* nil)
             (setq actl:*bridge-error*
                   (vl-catch-all-error-message actl:outcome)))
           (progn
             (setq actl:*bridge-value* (cadr actl:outcome))
             (setq actl:*bridge-status* T)
             (setq actl:*bridge-error* nil)))

         (setq actl:*bridge-source* nil)
         (princ)))
      (eval actl:staged-form)))

  (setq actl:*bridge-staged-form* nil)
  (princ))
