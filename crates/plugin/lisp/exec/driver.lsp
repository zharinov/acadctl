(progn
  {{VALUE_EMITTER}}

  (defun actl:print (actl:value)
    ({{EMIT_VALUE_FUNCTION}} actl:value)
    actl:value)

  (defun actl:label (actl:text)
    (if (eq (type actl:text) 'STR)
      ({{OUTPUT_EVENT_FUNCTION}} {{LABEL_EVENT}} actl:text)
      ({{OUTPUT_EVENT_FUNCTION}} {{INVALID_LABEL_EVENT}} nil))
    nil)

  (defun {{EMIT_RETAINED_VALUE_FUNCTION}} (/ actl:value actl:outcome)
    (setq actl:value {{VALUE_SYMBOL}})
    (setq {{VALUE_SYMBOL}} nil)
    (setq actl:outcome
      (vl-catch-all-apply
        '(lambda () ({{EMIT_VALUE_FUNCTION}} actl:value))
        '()))
    (setq {{ERRNO_SYMBOL}} (getvar "ERRNO"))

    (if (vl-catch-all-error-p actl:outcome)
      (progn
        (setq {{STATUS_SYMBOL}} nil)
        (setq {{ERROR_SYMBOL}}
          (vl-catch-all-error-message actl:outcome)))
      (progn
        (setq {{STATUS_SYMBOL}} T)
        (setq {{ERROR_SYMBOL}} nil)))

    (princ))

  (defun {{DRIVE_EXECUTION_FUNCTION}} (/ actl:continue actl:staged-form)
    (while (setq actl:continue ({{ADVANCE_EXECUTION_FUNCTION}}))
      (setq actl:staged-form (read {{STAGED_FORM_SYMBOL}}))
      (eval actl:staged-form))

    (setq {{STAGED_FORM_SYMBOL}} nil)
    (princ)))
