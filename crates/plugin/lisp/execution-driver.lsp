(defun acadctl:println (acadctl:value / acadctl:visitor)
  (setq acadctl:*bridge-value* acadctl:value)

  (if (acadctl:_begin-println)
    (progn
      (setq acadctl:visitor (read acadctl:*bridge-staged-form*))
      (eval acadctl:visitor)
      (acadctl:_finish-println))
    nil))

(defun acadctl:_drive-execution (/ acadctl:continue acadctl:staged-form)
  (while (setq acadctl:continue (acadctl:_advance-execution))
    (setq acadctl:staged-form (read acadctl:*bridge-staged-form*))
    (eval acadctl:staged-form))

  (setq acadctl:*bridge-staged-form* nil)
  (princ))
