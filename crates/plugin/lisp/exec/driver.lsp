(defun {{DRIVE_EXECUTION_FUNCTION}} (/ acadctl:continue acadctl:staged-form)
  (while (setq acadctl:continue ({{ADVANCE_EXECUTION_FUNCTION}}))
    (setq acadctl:staged-form (read {{STAGED_FORM_SYMBOL}}))
    (eval acadctl:staged-form))

  (setq {{STAGED_FORM_SYMBOL}} nil)
  (princ))
