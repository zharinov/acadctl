(defun actl:summary
  (/ add-count count-records outcome seed-counts validate-result)
  (setq validate-result
        '(lambda (result unavailable)
           (cond
             ((and (eq (type result) 'LIST)
                   (eq (car result) 'ok))
              nil)
             ((and (eq (type result) 'LIST)
                   (eq (car result) 'error))
              result)
             (T
              (actl:err
                (list
                  (strcat
                    "Could not summarize the drawing: "
                    unavailable)))))))

  (setq seed-counts
        '(lambda (names / counts name)
           (foreach name names
             (setq counts (cons (cons name 0) counts)))
           (reverse counts)))

  (setq add-count
        '(lambda (counts key amount / current)
           (if (setq current (assoc key counts))
             (subst
               (cons key (+ (cdr current) amount))
               current
               counts)
             (cons (cons key amount) counts))))

  (setq count-records
        '(lambda (counts field / output row)
           (foreach row counts
             (setq output
                   (cons
                     (list
                       (cons field (car row))
                       (cons 'count (cdr row)))
                     output)))
           (reverse output)))

  (setq outcome
        (vl-catch-all-apply
          '(lambda
             (/ color-count
              count
              dictionary-entries
              dictionary-names
              dictionary-result
              drawing-result
              group-names
              groups
              groups-result
              layer
              layer-count
              layer-counts
              layer-name
              layer-names
              layers
              layers-result
              layout-counts
              layout-name
              layout-names
              linetype-count
              lineweight-count
              malformed
              override
              row
              state
              total
              type-counts
              type-name)
              (setq drawing-result (actl:dwg))
              (setq state
                    (apply
                      validate-result
                      (list drawing-result "drawing facts are unavailable")))

              (if (null state)
                (progn
                  (setq layers-result (actl:layers))
                  (setq state
                        (apply
                          validate-result
                          (list layers-result "layer facts are unavailable")))))

              (if (null state)
                (progn
                  (setq groups-result (actl:groups))
                  (setq state
                        (apply
                          validate-result
                          (list groups-result "group facts are unavailable")))))

              (if (null state)
                (progn
                  (setq dictionary-result (actl:dict nil 0))
                  (setq state
                        (apply
                          validate-result
                          (list
                            dictionary-result
                            "the named-object dictionary is unavailable")))))

              (if state
                state
                (progn
                  (setq layers
                        (cdr (assoc 'items (cdr layers-result))))
                  (setq groups
                        (cdr (assoc 'items (cdr groups-result))))
                  (setq dictionary-entries
                        (cdr
                          (assoc
                            'entries
                            (cdr dictionary-result))))

                  (if (or
                        (null (assoc 'items (cdr layers-result)))
                        (null (assoc 'items (cdr groups-result)))
                        (null
                          (assoc
                            'entries
                            (cdr dictionary-result))))
                    (setq malformed T))

                  (setq total 0)
                  (setq color-count 0)
                  (setq linetype-count 0)
                  (setq lineweight-count 0)

                  (setq layout-names
                        (vl-sort
                          (cons "Model" (layoutlist))
                          '<))
                  (setq layout-counts
                        (apply seed-counts (list layout-names)))

                  (foreach layer layers
                    (setq layer-name (cdr (assoc 'name layer)))
                    (if (not (eq (type layer-name) 'STR))
                      (setq malformed T)
                      (progn
                        (setq layer-names
                              (cons layer-name layer-names))
                        (setq layer-count 0)
                        (foreach row (cdr (assoc 'counts layer))
                          (setq layout-name (cdr (assoc 'layout row)))
                          (setq type-name (cdr (assoc 'type row)))
                          (setq count (cdr (assoc 'count row)))
                          (if (not
                                (and
                                  (eq (type layout-name) 'STR)
                                  (assoc layout-name layout-counts)
                                  (eq (type type-name) 'STR)
                                  (eq (type count) 'INT)
                                  (>= count 0)))
                            (setq malformed T)
                            (progn
                              (setq total (+ total count))
                              (setq layer-count (+ layer-count count))
                              (setq layout-counts
                                    (apply
                                      add-count
                                      (list
                                        layout-counts
                                        layout-name
                                        count)))
                              (setq type-counts
                                    (apply
                                      add-count
                                      (list type-counts type-name count))))))
                        (setq layer-counts
                              (cons
                                (cons layer-name layer-count)
                                layer-counts))

                        (setq override (assoc 'overrides layer))
                        (if (not
                              (and
                                override
                                (eq
                                  (type (cdr (assoc 'color (cdr override))))
                                  'INT)
                                (eq
                                  (type (cdr (assoc 'linetype (cdr override))))
                                  'INT)
                                (eq
                                  (type (cdr (assoc 'lineweight (cdr override))))
                                  'INT)
                                (>=
                                  (cdr (assoc 'color (cdr override)))
                                  0)
                                (>=
                                  (cdr (assoc 'linetype (cdr override)))
                                  0)
                                (>=
                                  (cdr (assoc 'lineweight (cdr override)))
                                  0)))
                          (setq malformed T)
                          (progn
                            (setq color-count
                                  (+
                                    color-count
                                    (cdr
                                      (assoc
                                        'color
                                        (cdr override)))))
                            (setq linetype-count
                                  (+
                                    linetype-count
                                    (cdr
                                      (assoc
                                        'linetype
                                        (cdr override)))))
                            (setq lineweight-count
                                  (+
                                    lineweight-count
                                    (cdr
                                      (assoc
                                        'lineweight
                                        (cdr override))))))))))

                  (foreach layer-name layer-names
                    (if (member layer-name (cdr (member layer-name layer-names)))
                      (setq malformed T)))

                  (foreach layer groups
                    (setq layer-name (cdr (assoc 'name layer)))
                    (if (eq (type layer-name) 'STR)
                      (setq group-names (cons layer-name group-names))
                      (setq malformed T)))

                  (foreach layer dictionary-entries
                    (setq layer-name (cdr (assoc 'key layer)))
                    (if (eq (type layer-name) 'STR)
                      (setq dictionary-names
                            (cons layer-name dictionary-names))
                      (setq malformed T)))

                  (if malformed
                    (actl:err
                      (list
                        "Could not summarize the drawing: inspection facts are malformed"))
                    (progn
                      (setq layer-counts
                            (vl-sort layer-counts
                              '(lambda (left right)
                                 (< (car left) (car right)))))
                      (setq type-counts
                            (vl-sort type-counts
                              '(lambda (left right)
                                 (< (car left) (car right)))))
                      (setq group-names (reverse group-names))
                      (setq dictionary-names
                            (reverse dictionary-names))

                      (actl:ok
                        (list
                          (cons 'drawing (cdr drawing-result))
                          (list
                            'entities
                            (cons 'total total)
                            (cons
                              'by-layout
                              (apply
                                count-records
                                (list layout-counts 'layout)))
                            (cons
                              'by-layer
                              (apply
                                count-records
                                (list layer-counts 'layer)))
                            (cons
                              'by-type
                              (apply
                                count-records
                                (list type-counts 'type))))
                          (list
                            'overrides
                            (cons 'color color-count)
                            (cons 'linetype linetype-count)
                            (cons 'lineweight lineweight-count))
                          (list
                            'groups
                            (cons 'count (length group-names))
                            (cons 'names group-names))
                          (list
                            'named-object-dictionary
                            (cons 'count (length dictionary-names))
                            (cons 'names dictionary-names)))))))))
          '()))

  (if (vl-catch-all-error-p outcome)
    (actl:err
      (list
        (strcat
          "Could not summarize the drawing: "
          (vl-catch-all-error-message outcome))))
    outcome))
