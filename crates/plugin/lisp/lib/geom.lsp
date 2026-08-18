(defun actl:geom
  (subject /
   arc-record
   circle-record
   common
   data
   dxf-bulge-code
   dxf-center-code
   dxf-count-code
   dxf-elevation-code
   dxf-end-angle-code
   dxf-end-code
   dxf-entity-name-code
   dxf-flags-code
   dxf-normal-code
   dxf-radius-code
   dxf-start-angle-code
   dxf-start-code
   dxf-type-code
   entity-type
   full-turn-radians
   handle
   inspect
   line-record
   normal-value
   number-value-p
   outcome
   point-record
   point-value-p
   polyline-record
   reference)
  (setq dxf-bulge-code 42)
  (setq dxf-center-code 10)
  (setq dxf-count-code 90)
  (setq dxf-elevation-code 38)
  (setq dxf-end-angle-code 51)
  (setq dxf-end-code 11)
  (setq dxf-entity-name-code -1)
  (setq dxf-flags-code 70)
  (setq dxf-normal-code 210)
  (setq dxf-radius-code 40)
  (setq dxf-start-angle-code 50)
  (setq dxf-start-code 10)
  (setq dxf-type-code 0)
  (setq full-turn-radians (* 8.0 (atan 1.0)))

  (setq number-value-p
        '(lambda (value)
           (or (eq (type value) 'INT)
               (eq (type value) 'REAL))))

  (setq point-value-p
        '(lambda (value)
           (and (listp value)
                (apply number-value-p (list (car value)))
                (apply number-value-p (list (cadr value)))
                (apply number-value-p (list (caddr value)))
                (null (cdddr value)))))

  (setq normal-value
        '(lambda (data / pair value)
           (if (setq pair (assoc dxf-normal-code data))
             (setq value (cdr pair))
             (setq value '(0.0 0.0 1.0)))
           (if (and
                 (apply point-value-p (list value))
                 (/= (distance '(0.0 0.0 0.0) value) 0.0))
             (list
               value
               (if pair 'stored 'default))
             (actl:err
               "The entity has a malformed extrusion normal"))))

  (setq common
        '(lambda (kind)
           (list
             (cons 'handle handle)
             (cons 'type entity-type)
             (cons 'kind kind))))

  (setq line-record
        '(lambda (/ end start)
           (setq start (cdr (assoc dxf-start-code data)))
           (setq end (cdr (assoc dxf-end-code data)))
           (if (not (and
                      (apply point-value-p (list start))
                      (apply point-value-p (list end))))
             (actl:err
               "The LINE has malformed or missing endpoints")
             (actl:ok
               (append
                 (apply common (list 'line))
                 (list
                   (cons 'coordinates 'wcs)
                   (cons 'from start)
                   (cons 'to end)))))))

  (setq point-record
        '(lambda (/ position)
           (setq position (cdr (assoc dxf-start-code data)))
           (if (not (apply point-value-p (list position)))
             (actl:err
               "The POINT has a malformed or missing position")
             (actl:ok
               (append
                 (apply common (list 'point))
                 (list
                   (cons 'coordinates 'wcs)
                   (cons 'position position)))))))

  (setq circle-record
        '(lambda (/ center center-wcs ename normal normal-result radius)
           (setq center (cdr (assoc dxf-center-code data)))
           (setq radius (cdr (assoc dxf-radius-code data)))
           (setq ename (cdr (assoc dxf-entity-name-code data)))
           (setq normal-result (apply normal-value (list data)))
           (cond
             ((not (apply point-value-p (list center)))
              (actl:err
                "The CIRCLE has a malformed or missing center"))
             ((not (and
                     (apply number-value-p (list radius))
                     (> radius 0.0)))
              (actl:err
                "The CIRCLE has a malformed or invalid radius"))
             ((eq (car normal-result) 'error) normal-result)
             ((null ename)
              (actl:err
                "The CIRCLE has no entity coordinate system"))
             (T
              (setq normal (car normal-result))
              (setq center-wcs (trans center ename 0))
              (actl:ok
                (append
                  (apply common (list 'circle))
                  (list
                    (cons 'coordinates 'wcs)
                    (cons 'source-coordinates 'ocs)
                    (cons 'normal normal)
                    (cons 'normal-source (cadr normal-result))
                    (cons 'center-ocs center)
                    (cons 'center center-wcs)
                    (cons 'radius radius))))))))

  (setq arc-record
        '(lambda
           (/ center center-wcs end-angle ename from from-ocs normal normal-result radius start-angle sweep to to-ocs)
           (setq center (cdr (assoc dxf-center-code data)))
           (setq radius (cdr (assoc dxf-radius-code data)))
           (setq start-angle (cdr (assoc dxf-start-angle-code data)))
           (setq end-angle (cdr (assoc dxf-end-angle-code data)))
           (setq ename (cdr (assoc dxf-entity-name-code data)))
           (setq normal-result (apply normal-value (list data)))
           (cond
             ((not (apply point-value-p (list center)))
              (actl:err
                "The ARC has a malformed or missing center"))
             ((not (and
                     (apply number-value-p (list radius))
                     (> radius 0.0)))
              (actl:err
                "The ARC has a malformed or invalid radius"))
             ((not (and
                     (apply number-value-p (list start-angle))
                     (apply number-value-p (list end-angle))))
              (actl:err
                "The ARC has malformed or missing angles"))
             ((eq (car normal-result) 'error) normal-result)
             ((null ename)
              (actl:err
                "The ARC has no entity coordinate system"))
             (T
              (setq normal (car normal-result))
              (setq center-wcs (trans center ename 0))
              (setq from-ocs
                    (list
                      (+ (car center) (* radius (cos start-angle)))
                      (+ (cadr center) (* radius (sin start-angle)))
                      (caddr center)))
              (setq to-ocs
                    (list
                      (+ (car center) (* radius (cos end-angle)))
                      (+ (cadr center) (* radius (sin end-angle)))
                      (caddr center)))
              (setq from (trans from-ocs ename 0))
              (setq to (trans to-ocs ename 0))
              (setq sweep (- end-angle start-angle))
              (while (< sweep 0.0)
                (setq sweep (+ sweep full-turn-radians)))
              (while (>= sweep full-turn-radians)
                (setq sweep (- sweep full-turn-radians)))
              (actl:ok
                (append
                  (apply common (list 'circular-arc))
                  (list
                    (cons 'coordinates 'wcs)
                    (cons 'source-coordinates 'ocs)
                    (cons 'normal normal)
                    (cons 'normal-source (cadr normal-result))
                    (cons 'center-ocs center)
                    (cons 'center center-wcs)
                    (cons 'radius radius)
                    (cons 'start-angle-radians start-angle)
                    (cons 'end-angle-radians end-angle)
                    (cons 'from from)
                    (cons 'to to)
                    (cons 'sweep-radians sweep))))))))

  (setq polyline-record
        '(lambda
           (/ bulge bulge-seen bulge-source center center-ocs chord closed count count-pair current dx dy elevation elevation-pair ename flags flags-pair from from-ocs header-count index malformed midpoint-x midpoint-y normal normal-pair offset pair position position-ocs radius raw-vertices segment-count segments state sweep to to-ocs vertices)
           (setq count-pair (assoc dxf-count-code data))
           (setq flags-pair (assoc dxf-flags-code data))
           (setq elevation-pair (assoc dxf-elevation-code data))
           (setq normal-pair (assoc dxf-normal-code data))
           (setq count (if count-pair (cdr count-pair)))
           (setq flags (if flags-pair (cdr flags-pair) 0))
           (setq elevation
                 (if elevation-pair (cdr elevation-pair) 0.0))
           (setq normal
                 (if normal-pair
                   (cdr normal-pair)
                   '(0.0 0.0 1.0)))
           (setq ename (cdr (assoc dxf-entity-name-code data)))

           (setq header-count 0)
           (foreach pair data
             (if (= (car pair) dxf-count-code)
               (setq header-count (1+ header-count))))
           (cond
             ((or (/= header-count 1)
                  (not (eq (type count) 'INT))
                  (< count 0))
              (setq state
                    (actl:err
                      "The LWPOLYLINE has a malformed or missing vertex count")))
             ((not (eq (type flags) 'INT))
              (setq state
                    (actl:err
                      "The LWPOLYLINE has malformed flags")))
             ((not (apply number-value-p (list elevation)))
              (setq state
                    (actl:err
                      "The LWPOLYLINE has a malformed elevation")))
             ((not (and
                     (apply point-value-p (list normal))
                     (/= (distance '(0.0 0.0 0.0) normal) 0.0)))
              (setq state
                    (actl:err
                      "The LWPOLYLINE has a malformed extrusion normal")))
             ((null ename)
              (setq state
                    (actl:err
                      "The LWPOLYLINE has no entity coordinate system"))))

           (if (null state)
             (progn
               (foreach pair data
                 (cond
                   ((= (car pair) dxf-start-code)
                    (if current
                      (setq raw-vertices
                            (cons
                              (list current bulge bulge-source)
                              raw-vertices)))
                    (setq current (cdr pair))
                    (setq bulge 0.0)
                    (setq bulge-source 'default)
                    (setq bulge-seen nil))
                   ((= (car pair) dxf-bulge-code)
                    (if (or (null current) bulge-seen)
                      (setq malformed T)
                      (progn
                        (setq bulge (cdr pair))
                        (setq bulge-source 'stored)
                        (setq bulge-seen T))))))
               (if current
                 (setq raw-vertices
                       (cons
                         (list current bulge bulge-source)
                         raw-vertices)))
               (setq raw-vertices (reverse raw-vertices))
               (if (or malformed (/= count (length raw-vertices)))
                 (setq state
                       (actl:err
                         "The LWPOLYLINE has malformed vertex records")))))

           (if (null state)
             (progn
               (setq index 0)
               (foreach current raw-vertices
                 (setq position (car current))
                 (setq bulge (cadr current))
                 (setq bulge-source (caddr current))
                 (if (not (and
                            (listp position)
                            (apply number-value-p (list (car position)))
                            (apply number-value-p (list (cadr position)))
                            (null (cddr position))
                            (apply number-value-p (list bulge))))
                   (setq state
                         (actl:err
                           "The LWPOLYLINE has malformed vertex coordinates or bulges"))
                   (progn
                     (setq position-ocs
                           (list
                             (car position)
                             (cadr position)
                             elevation))
                     (setq vertices
                           (cons
                             (list
                               (cons 'observed-index index)
                               (cons 'position-ocs position-ocs)
                               (cons
                                 'position
                                 (trans position-ocs ename 0))
                               (cons 'bulge bulge)
                               (cons 'bulge-source bulge-source))
                             vertices))))
                 (setq index (1+ index)))
               (setq vertices (reverse vertices))))

           (if (null state)
             (progn
               (setq closed (/= (logand flags 1) 0))
               (setq segment-count
                     (cond
                       ((< count 2) 0)
                       (closed count)
                       (T (1- count))))
               (setq index 0)
               (while (and (< index segment-count) (null state))
                 (setq from (nth index vertices))
                 (setq to (nth (rem (1+ index) count) vertices))
                 (setq from-ocs (cdr (assoc 'position-ocs from)))
                 (setq to-ocs (cdr (assoc 'position-ocs to)))
                 (setq bulge (cdr (assoc 'bulge from)))
                 (setq bulge-source
                       (cdr (assoc 'bulge-source from)))
                 (setq dx (- (car to-ocs) (car from-ocs)))
                 (setq dy (- (cadr to-ocs) (cadr from-ocs)))
                 (setq chord (sqrt (+ (* dx dx) (* dy dy))))
                 (if (= bulge 0.0)
                   (setq segments
                         (cons
                           (list
                             (cons 'observed-index index)
                             (cons 'kind 'line)
                             (cons
                               'representation
                               'lwpolyline-zero-bulge)
                             (cons 'bulge bulge)
                             (cons 'bulge-source bulge-source)
                             (cons 'from (cdr (assoc 'position from)))
                             (cons 'to (cdr (assoc 'position to)))
                             (cons 'chord-length chord))
                           segments))
                   (if (= chord 0.0)
                     (setq state
                           (actl:err
                             "The LWPOLYLINE has a curved segment with coincident endpoints"))
                     (progn
                       (setq midpoint-x
                             (/ (+ (car from-ocs) (car to-ocs)) 2.0))
                       (setq midpoint-y
                             (/ (+ (cadr from-ocs) (cadr to-ocs)) 2.0))
                       (setq offset
                             (/ (* chord (- 1.0 (* bulge bulge)))
                                (* 4.0 bulge)))
                       (setq center-ocs
                             (list
                               (- midpoint-x (* (/ dy chord) offset))
                               (+ midpoint-y (* (/ dx chord) offset))
                               elevation))
                       (setq center (trans center-ocs ename 0))
                       (setq radius
                             (/ (* chord (+ 1.0 (* bulge bulge)))
                                (* 4.0 (abs bulge))))
                       (setq sweep (* 4.0 (atan bulge)))
                       (setq segments
                             (cons
                               (list
                                 (cons 'observed-index index)
                                 (cons 'kind 'circular-arc)
                                 (cons
                                   'representation
                                   'lwpolyline-bulge)
                                 (cons 'bulge bulge)
                                 (cons 'bulge-source bulge-source)
                                 (cons
                                   'from
                                   (cdr (assoc 'position from)))
                                 (cons
                                   'to
                                   (cdr (assoc 'position to)))
                                 (cons 'chord-length chord)
                                 (cons 'center center)
                                 (cons 'radius radius)
                                 (cons 'sweep-radians sweep))
                               segments)))))
                 (setq index (1+ index)))
               (setq segments (reverse segments))))

           (if state
             state
             (actl:ok
               (append
                 (apply common (list 'polyline))
                 (list
                   (cons 'coordinates 'wcs)
                   (cons 'source-coordinates 'ocs)
                   (cons 'closed (if closed T nil))
                   (cons 'elevation elevation)
                   (cons
                     'elevation-source
                     (if elevation-pair 'stored 'default))
                   (cons 'normal normal)
                   (cons
                     'normal-source
                     (if normal-pair 'stored 'default))
                   (cons 'vertices vertices)
                   (cons 'segments segments)))))))

  (setq inspect
        '(lambda (resolved / value)
           (setq handle (cdr (assoc 'handle resolved)))
           (setq data (cdr (assoc 'value resolved)))
           (setq entity-type (cdr (assoc dxf-type-code data)))
           (cond
             ((= entity-type "LINE") (apply line-record '()))
             ((= entity-type "POINT") (apply point-record '()))
             ((= entity-type "CIRCLE") (apply circle-record '()))
             ((= entity-type "ARC") (apply arc-record '()))
             ((= entity-type "LWPOLYLINE") (apply polyline-record '()))
             (T
              (actl:err
                (strcat
                  "Unsupported geometry type: "
                  (if entity-type entity-type "unknown")))))))

  (setq reference (actl:dxf subject))
  (cond
    ((null reference) nil)
    ((eq (car reference) 'error) reference)
    (T
     (setq outcome
           (vl-catch-all-apply inspect (list (cdr reference))))
     (if (vl-catch-all-error-p outcome)
       (actl:err
         (strcat
           "Could not inspect geometry: "
           (vl-catch-all-error-message outcome)))
       outcome))))
