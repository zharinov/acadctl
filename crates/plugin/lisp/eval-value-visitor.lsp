(progn
  ((lambda (/ acadctl:value
              acadctl:outcome
              acadctl:continue
              acadctl:stack
              acadctl:task
              acadctl:task-kind
              acadctl:current
              acadctl:current-type
              acadctl:depth
              acadctl:tail
              acadctl:slow
              acadctl:fast
              acadctl:next-slow
              acadctl:next-fast
              acadctl:atom-text
              acadctl:text
              acadctl:text-offset)
     (setq acadctl:value acadctl:*bridge-value*)
     (setq acadctl:*bridge-value* nil)

     (setq acadctl:outcome
       (vl-catch-all-apply
         '(lambda ()
            (setq acadctl:continue T)
            (setq acadctl:stack (list (list 0 acadctl:value 0)))

            (while (and acadctl:continue acadctl:stack)
              (setq acadctl:task (car acadctl:stack))
              (setq acadctl:stack (cdr acadctl:stack))
              (setq acadctl:task-kind (car acadctl:task))

              (cond
                ((= acadctl:task-kind 0)
                  (setq acadctl:current (cadr acadctl:task))
                  (setq acadctl:depth (caddr acadctl:task))

                  (cond
                    ((null acadctl:current)
                      (setq acadctl:continue
                        ({{CALLBACK}} {{NIL}} nil)))
                    ((eq acadctl:current T)
                      (setq acadctl:continue
                        ({{CALLBACK}} {{TRUE}} nil)))
                    ((vl-catch-all-error-p acadctl:current)
                      (setq acadctl:continue
                        ({{CALLBACK}} {{ERROR_OBJECT}} nil)))
                    ((vl-consp acadctl:current)
                      (if (>= acadctl:depth {{MAX_DEPTH}})
                        (setq acadctl:continue
                          ({{CALLBACK}} {{TOO_DEEP}} nil))
                        (progn
                          (setq acadctl:continue
                            ({{CALLBACK}} {{BEGIN_LIST}} nil))
                          (if acadctl:continue
                            (setq acadctl:stack
                              (cons
                                (list 1
                                      acadctl:current
                                      acadctl:current
                                      acadctl:current
                                      acadctl:depth)
                                acadctl:stack))))))
                    (T
                      (setq acadctl:current-type (type acadctl:current))
                      (cond
                        ((eq acadctl:current-type 'INT)
                          (setq acadctl:continue
                            ({{CALLBACK}} {{INTEGER}} acadctl:current)))
                        ((eq acadctl:current-type 'REAL)
                          (setq acadctl:continue
                            ({{CALLBACK}} {{REAL}} acadctl:current)))
                        ((eq acadctl:current-type 'STR)
                          (setq acadctl:continue
                            ({{CALLBACK}} {{BEGIN_STRING}} nil))
                          (if acadctl:continue
                            (progn
                              (setq acadctl:text-offset 1)
                              (setq acadctl:text
                                (substr acadctl:current
                                        acadctl:text-offset
                                        {{CHUNK_CHARS}}))

                              (while
                                (and acadctl:continue
                                     (/= acadctl:text ""))
                                (setq acadctl:continue
                                  ({{CALLBACK}}
                                    {{STRING_CHUNK}}
                                    acadctl:text))
                                (setq acadctl:text-offset
                                  (+ acadctl:text-offset {{CHUNK_CHARS}}))
                                (if acadctl:continue
                                  (setq acadctl:text
                                    (substr acadctl:current
                                            acadctl:text-offset
                                            {{CHUNK_CHARS}}))))

                              (if acadctl:continue
                                (setq acadctl:continue
                                  ({{CALLBACK}} {{END_STRING}} nil))))))
                        ((eq acadctl:current-type 'SYM)
                          (setq acadctl:continue
                            ({{CALLBACK}} {{BEGIN_SYMBOL}} nil))
                          (if acadctl:continue
                            (progn
                              (setq acadctl:atom-text
                                (vl-symbol-name acadctl:current))
                              (setq acadctl:text-offset 1)
                              (setq acadctl:text
                                (substr acadctl:atom-text
                                        acadctl:text-offset
                                        {{CHUNK_CHARS}}))

                              (while
                                (and acadctl:continue
                                     (/= acadctl:text ""))
                                (setq acadctl:continue
                                  ({{CALLBACK}}
                                    {{SYMBOL_CHUNK}}
                                    acadctl:text))
                                (setq acadctl:text-offset
                                  (+ acadctl:text-offset {{CHUNK_CHARS}}))
                                (if acadctl:continue
                                  (setq acadctl:text
                                    (substr acadctl:atom-text
                                            acadctl:text-offset
                                            {{CHUNK_CHARS}}))))

                              (if acadctl:continue
                                (setq acadctl:continue
                                  ({{CALLBACK}} {{END_SYMBOL}} nil))))))
                        ((eq acadctl:current-type 'ENAME)
                          (setq acadctl:continue
                            ({{CALLBACK}} {{ENTITY}} acadctl:current)))
                        ((eq acadctl:current-type 'PICKSET)
                          (setq acadctl:continue
                            ({{CALLBACK}} {{SELECTION_SET}} nil)))
                        ((eq acadctl:current-type 'VLA-OBJECT)
                          (setq acadctl:continue
                            ({{CALLBACK}} {{VLA_OBJECT}} nil)))
                        ((eq acadctl:current-type 'FILE)
                          (setq acadctl:continue
                            ({{CALLBACK}} {{FILE}} nil)))
                        ((or (eq acadctl:current-type 'SUBR)
                             (eq acadctl:current-type 'USUBR)
                             (eq acadctl:current-type 'EXRXSUBR))
                          (setq acadctl:continue
                            ({{CALLBACK}} {{FUNCTION}} nil)))
                        (T
                          (setq acadctl:continue
                            ({{CALLBACK}}
                              {{OBJECT}}
                              (vl-symbol-name acadctl:current-type))))))))
                ((= acadctl:task-kind 1)
                  (setq acadctl:tail (cadr acadctl:task))
                  (setq acadctl:slow (caddr acadctl:task))
                  (setq acadctl:fast (cadddr acadctl:task))
                  (setq acadctl:depth (car (cddddr acadctl:task)))

                  (cond
                    ((null acadctl:tail)
                      (setq acadctl:continue
                        ({{CALLBACK}} {{END_LIST}} nil)))
                    ((vl-consp acadctl:tail)
                      (setq acadctl:next-slow
                        (if (vl-consp acadctl:slow)
                          (cdr acadctl:slow)
                          nil))
                      (setq acadctl:next-fast
                        (if (and (vl-consp acadctl:fast)
                                 (vl-consp (cdr acadctl:fast)))
                          (cdr (cdr acadctl:fast))
                          nil))
                      (if (and (vl-consp acadctl:next-slow)
                               (eq acadctl:next-slow acadctl:next-fast))
                        (setq acadctl:stack
                          (cons (list 0 (car acadctl:tail) (+ acadctl:depth 1))
                            (cons (list 3)
                              (cons (list 4)
                                (cons (list 2) acadctl:stack)))))
                        (setq acadctl:stack
                          (cons (list 0 (car acadctl:tail) (+ acadctl:depth 1))
                            (cons
                              (list 1
                                    (cdr acadctl:tail)
                                    acadctl:next-slow
                                    acadctl:next-fast
                                    acadctl:depth)
                              acadctl:stack)))))
                    (T
                      (setq acadctl:continue
                        ({{CALLBACK}} {{DOT}} nil))
                      (if acadctl:continue
                        (setq acadctl:stack
                          (cons
                            (list 0 acadctl:tail (+ acadctl:depth 1))
                            (cons (list 2) acadctl:stack)))))))
                ((= acadctl:task-kind 2)
                  (setq acadctl:continue
                    ({{CALLBACK}} {{END_LIST}} nil)))
                ((= acadctl:task-kind 3)
                  (setq acadctl:continue
                    ({{CALLBACK}} {{DOT}} nil)))
                ((= acadctl:task-kind 4)
                  (setq acadctl:continue
                    ({{CALLBACK}} {{CYCLE}} nil)))
                (T
                  (acadctl:_invalid-value-task)))
            T))
         '()))

     (setq acadctl:*bridge-errno* (getvar "ERRNO"))

     (if (vl-catch-all-error-p acadctl:outcome)
       (progn
         (setq acadctl:*bridge-status* nil)
         (setq acadctl:*bridge-error*
           (vl-catch-all-error-message acadctl:outcome)))
       (progn
         (setq acadctl:*bridge-status* T)
         (setq acadctl:*bridge-error* nil)))))

  (setq acadctl:*bridge-value* nil)
  (princ))
