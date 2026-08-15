(defun acadctl:println (acadctl:value / acadctl:visitor)
  (setq {{VALUE_SYMBOL}} acadctl:value)

  (if ({{BEGIN_PRINTLN_FUNCTION}})
    (progn
      (setq acadctl:visitor (read {{STAGED_FORM_SYMBOL}}))
      (eval acadctl:visitor)
      ({{FINISH_PRINTLN_FUNCTION}}))
    nil))

(defun {{DRIVE_EXECUTION_FUNCTION}} (/ acadctl:continue acadctl:staged-form)
  (while (setq acadctl:continue ({{ADVANCE_EXECUTION_FUNCTION}}))
    (setq acadctl:staged-form (read {{STAGED_FORM_SYMBOL}}))
    (eval acadctl:staged-form))

  (setq {{STAGED_FORM_SYMBOL}} nil)
  (princ))
