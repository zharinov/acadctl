(defun actl:_drive-execution (/ expected-form-count staged-form)
  (setq expected-form-count 1)
  (while (actl:_advance-execution)
    (setq staged-form (read actl:*bridge-staged-form*))
    (if (eq staged-form 'actl:_eval)
      ((lambda (/ forms outcome)
         (setq outcome
               (vl-catch-all-apply
                 '(lambda ()
                    (setq forms
                          (read (strcat "(" actl:*bridge-source* "\n)")))
                    (if (= (length forms) expected-form-count)
                      (list 'actl:ok (eval (car forms)))
                      (actl:_invalid-form-span)))
                 '()))

         (setq actl:*bridge-errno* (getvar "ERRNO"))

         (if (vl-catch-all-error-p outcome)
           (progn
             (setq actl:*bridge-status* nil)
             (setq actl:*bridge-error*
                   (vl-catch-all-error-message outcome)))
           (progn
             (setq actl:*bridge-value* (cadr outcome))
             (setq actl:*bridge-status* T)
             (setq actl:*bridge-error* nil)))

         (setq actl:*bridge-source* nil)
         (princ)))
      (eval staged-form)))

  (setq actl:*bridge-staged-form* nil)
  (princ))
