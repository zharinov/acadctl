(defun {{EMIT_VALUE_FUNCTION}} (actl:value /
                                 actl:continue
                                 actl:stack
                                 actl:task
                                 actl:task-kind
                                 actl:current
                                 actl:current-type
                                 actl:depth
                                 actl:tail
                                 actl:slow
                                 actl:fast
                                 actl:next-slow
                                 actl:next-fast
                                 actl:atom-text
                                 actl:text
                                 actl:text-offset)
  (setq actl:continue
    ({{CALLBACK}} {{BEGIN_VALUE}} nil))
  (setq actl:stack
    (if actl:continue
      (list (list 0 actl:value 0))))

            (while (and actl:continue actl:stack)
              (setq actl:task (car actl:stack))
              (setq actl:stack (cdr actl:stack))
              (setq actl:task-kind (car actl:task))

              (cond
                ((= actl:task-kind 0)
                  (setq actl:current (cadr actl:task))
                  (setq actl:depth (caddr actl:task))

                  (cond
                    ((null actl:current)
                      (setq actl:continue
                        ({{CALLBACK}} {{NIL}} nil)))
                    ((eq actl:current T)
                      (setq actl:continue
                        ({{CALLBACK}} {{TRUE}} nil)))
                    ((vl-catch-all-error-p actl:current)
                      (setq actl:continue
                        ({{CALLBACK}} {{ERROR_OBJECT}} nil)))
                    ((vl-consp actl:current)
                      (if (>= actl:depth {{MAX_DEPTH}})
                        (setq actl:continue
                          ({{CALLBACK}} {{TOO_DEEP}} nil))
                        (progn
                          (setq actl:continue
                            ({{CALLBACK}} {{BEGIN_LIST}} nil))
                          (if actl:continue
                            (setq actl:stack
                              (cons
                                (list 1
                                      actl:current
                                      actl:current
                                      actl:current
                                      actl:depth)
                                actl:stack))))))
                    (T
                      (setq actl:current-type (type actl:current))
                      (cond
                        ((eq actl:current-type 'INT)
                          (setq actl:continue
                            ({{CALLBACK}} {{INTEGER}} actl:current)))
                        ((eq actl:current-type 'REAL)
                          (setq actl:continue
                            ({{CALLBACK}} {{REAL}} actl:current)))
                        ((eq actl:current-type 'STR)
                          (setq actl:continue
                            ({{CALLBACK}} {{BEGIN_STRING}} nil))
                          (if actl:continue
                            (progn
                              (setq actl:text-offset 1)
                              (setq actl:text
                                (substr actl:current
                                        actl:text-offset
                                        {{CHUNK_CHARS}}))

                              (while
                                (and actl:continue
                                     (/= actl:text ""))
                                (setq actl:continue
                                  ({{CALLBACK}}
                                    {{STRING_CHUNK}}
                                    actl:text))
                                (setq actl:text-offset
                                  (+ actl:text-offset {{CHUNK_CHARS}}))
                                (if actl:continue
                                  (setq actl:text
                                    (substr actl:current
                                            actl:text-offset
                                            {{CHUNK_CHARS}}))))

                              (if actl:continue
                                (setq actl:continue
                                  ({{CALLBACK}} {{END_STRING}} nil))))))
                        ((eq actl:current-type 'SYM)
                          (setq actl:continue
                            ({{CALLBACK}} {{BEGIN_SYMBOL}} nil))
                          (if actl:continue
                            (progn
                              (setq actl:atom-text
                                (vl-symbol-name actl:current))
                              (setq actl:text-offset 1)
                              (setq actl:text
                                (substr actl:atom-text
                                        actl:text-offset
                                        {{CHUNK_CHARS}}))

                              (while
                                (and actl:continue
                                     (/= actl:text ""))
                                (setq actl:continue
                                  ({{CALLBACK}}
                                    {{SYMBOL_CHUNK}}
                                    actl:text))
                                (setq actl:text-offset
                                  (+ actl:text-offset {{CHUNK_CHARS}}))
                                (if actl:continue
                                  (setq actl:text
                                    (substr actl:atom-text
                                            actl:text-offset
                                            {{CHUNK_CHARS}}))))

                              (if actl:continue
                                (setq actl:continue
                                  ({{CALLBACK}} {{END_SYMBOL}} nil))))))
                        ((eq actl:current-type 'ENAME)
                          (setq actl:continue
                            ({{CALLBACK}} {{ENTITY}} actl:current)))
                        ((eq actl:current-type 'PICKSET)
                          (setq actl:continue
                            ({{CALLBACK}} {{SELECTION_SET}} nil)))
                        ((eq actl:current-type 'VLA-OBJECT)
                          (setq actl:continue
                            ({{CALLBACK}} {{VLA_OBJECT}} nil)))
                        ((eq actl:current-type 'FILE)
                          (setq actl:continue
                            ({{CALLBACK}} {{FILE}} nil)))
                        ((or (eq actl:current-type 'SUBR)
                             (eq actl:current-type 'USUBR)
                             (eq actl:current-type 'EXRXSUBR))
                          (setq actl:continue
                            ({{CALLBACK}} {{FUNCTION}} nil)))
                        (T
                          (setq actl:continue
                            ({{CALLBACK}}
                              {{OBJECT}}
                              (vl-symbol-name actl:current-type))))))))
                ((= actl:task-kind 1)
                  (setq actl:tail (cadr actl:task))
                  (setq actl:slow (caddr actl:task))
                  (setq actl:fast (cadddr actl:task))
                  (setq actl:depth (car (cddddr actl:task)))

                  (cond
                    ((null actl:tail)
                      (setq actl:continue
                        ({{CALLBACK}} {{END_LIST}} nil)))
                    ((vl-consp actl:tail)
                      (setq actl:next-slow
                        (if (vl-consp actl:slow)
                          (cdr actl:slow)
                          nil))
                      (setq actl:next-fast
                        (if (and (vl-consp actl:fast)
                                 (vl-consp (cdr actl:fast)))
                          (cdr (cdr actl:fast))
                          nil))
                      (if (and (vl-consp actl:next-slow)
                               (eq actl:next-slow actl:next-fast))
                        (setq actl:stack
                          (cons (list 0 (car actl:tail) (+ actl:depth 1))
                            (cons (list 3)
                              (cons (list 4)
                                (cons (list 2) actl:stack)))))
                        (setq actl:stack
                          (cons (list 0 (car actl:tail) (+ actl:depth 1))
                            (cons
                              (list 1
                                    (cdr actl:tail)
                                    actl:next-slow
                                    actl:next-fast
                                    actl:depth)
                              actl:stack)))))
                    (T
                      (setq actl:continue
                        ({{CALLBACK}} {{DOT}} nil))
                      (if actl:continue
                        (setq actl:stack
                          (cons
                            (list 0 actl:tail (+ actl:depth 1))
                            (cons (list 2) actl:stack)))))))
                ((= actl:task-kind 2)
                  (setq actl:continue
                    ({{CALLBACK}} {{END_LIST}} nil)))
                ((= actl:task-kind 3)
                  (setq actl:continue
                    ({{CALLBACK}} {{DOT}} nil)))
                ((= actl:task-kind 4)
                  (setq actl:continue
                    ({{CALLBACK}} {{CYCLE}} nil)))
                (T
                  (actl:_invalid-value-task)))
            )

  ({{CALLBACK}} {{END_VALUE}} nil)
  nil)
