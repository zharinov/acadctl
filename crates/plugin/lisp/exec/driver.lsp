(progn
  {{VALUE_EMITTER}}

  (defun acadctl:print (acadctl:value)
    ({{EMIT_VALUE_FUNCTION}} acadctl:value)
    acadctl:value)

  (defun acadctl:label (acadctl:text)
    (if (eq (type acadctl:text) 'STR)
      ({{OUTPUT_EVENT_FUNCTION}} {{LABEL_EVENT}} acadctl:text)
      ({{OUTPUT_EVENT_FUNCTION}} {{INVALID_LABEL_EVENT}} nil))
    nil)

  (defun {{EMIT_RETAINED_VALUE_FUNCTION}} (/ acadctl:value acadctl:outcome)
    (setq acadctl:value {{VALUE_SYMBOL}})
    (setq {{VALUE_SYMBOL}} nil)
    (setq acadctl:outcome
      (vl-catch-all-apply
        '(lambda () ({{EMIT_VALUE_FUNCTION}} acadctl:value))
        '()))
    (setq {{ERRNO_SYMBOL}} (getvar "ERRNO"))

    (if (vl-catch-all-error-p acadctl:outcome)
      (progn
        (setq {{STATUS_SYMBOL}} nil)
        (setq {{ERROR_SYMBOL}}
          (vl-catch-all-error-message acadctl:outcome)))
      (progn
        (setq {{STATUS_SYMBOL}} T)
        (setq {{ERROR_SYMBOL}} nil)))

    (princ))

  (defun {{DRIVE_EXECUTION_FUNCTION}} (/ acadctl:continue acadctl:staged-form)
    (while (setq acadctl:continue ({{ADVANCE_EXECUTION_FUNCTION}}))
      (setq acadctl:staged-form (read {{STAGED_FORM_SYMBOL}}))
      (eval acadctl:staged-form))

    (setq {{STAGED_FORM_SYMBOL}} nil)
    (princ)))
