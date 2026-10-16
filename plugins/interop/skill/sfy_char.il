;; sfy_char.il: SfyCharacterize, which measures what a PDK device's CDF callbacks derive from
;; the parameters a user sets (spec interop.md 8.3). The callbacks are SKILL, often
;; encrypted, and never cross to Schemify; their effect is measured here, on a scratch
;; schematic that is never saved, and written as .sfyx records:
;;   ("char" lib cell (input ...) (netlisted ...) (kind per input) (default per input))
;;   ("probe" candidate kind set-text status (netlisted params it changed ...))
;;   ("pt" (set value per input, "" at its default) status (value per netlisted)
;;         (value read back per input))
;; Nothing here knows a device type or a parameter name: the inputs are found by
;; experiment (a candidate is an input when nudging it changes another netlisted param),
;; or named with ?inputs. Kinds: "num", "count", "bool", "choice". Prefix form only, every
;; Cadence call inside errset.

;; A CDF number string ("80n", "1.5u", "2") as a float; nil when it is not one.
(procedure (SfyCharNum s)
  (let ((n (and (stringp s) (strlen s))) (scale 1.0) (c nil) (h nil))
    (cond
      ((numberp s) (float s))
      ((and n (greaterp n 0))
        (setq c (substring s n 1))
        (setq h (substring s 1 1))
        (setq scale (case c ("f" 1e-15) ("p" 1e-12) ("n" 1e-9) ("u" 1e-6) ("m" 1e-3)
                            ("k" 1e3) ("K" 1e3) ("M" 1e6) ("G" 1e9) (t 1.0)))
        (and (or (digitp h) (member h (list "-" "+" ".")))
             (if (equal scale 1.0)
               (and (or (digitp c) (equal c ".")) (atof s))
               (and (greaterp n 1) (times (atof (substring s 1 (sub1 n))) scale)))))
      (t nil))))

;; True when string s is a whole number written with digits only.
(procedure (SfyCharWhole s)
  (and (stringp s) (greaterp (strlen s) 0)
       (null (setof c (parseString s "") (not (digitp c))))))

;; The candidate p as (name kind default choices), or nil when it is not numeric, boolean
;; or a choice: an int is a "count", a string or float with a numeric default a "num" (a
;; "count" when written as a whole number), a boolean a "bool", a cyclic or radio a "choice".
(procedure (SfyCharCand p)
  (let ((ty (getSGq p paramType)) (d (getSGq p defValue)))
    (cond
      ((equal ty "int") (list (getSGq p name) "count" (if (fixp d) d (atoi (SfyAtomStr d))) nil))
      ((equal ty "boolean") (list (getSGq p name) "bool" (if (member d (list t "t" "TRUE" "true")) "t" "nil") nil))
      ((member ty (list "cyclic" "radio"))
        (and (getSGq p choices) (list (getSGq p name) "choice" (SfyAtomStr d) (mapcar 'SfyAtomStr (getSGq p choices)))))
      ((SfyCharWhole (SfyAtomStr d)) (list (getSGq p name) "count" (atoi (SfyAtomStr d)) nil))
      ((SfyCharNum (SfyAtomStr d)) (list (getSGq p name) "num" (SfyCharNum (SfyAtomStr d)) nil))
      (t nil))))

;; The text an input i (name kind default choices) is set to for value v (a boolean's
;; value is the text "t" or "nil": nil itself means "at its default").
(procedure (SfyCharText i v)
  (case (cadr i)
    ("count" (sprintf nil "%d" (round v)))
    ("num" (sprintf nil "%g" v))
    (t (SfyAtomStr v))))

;; The nudged value of input i: twice a number (a small one for zero), one more count,
;; the other boolean, the next choice.
(procedure (SfyCharNudge i)
  (let ((d (caddr i)) (cs (cadddr i)))
    (case (cadr i)
      ("num" (if (zerop d) 1e-6 (times 2 d)))
      ("count" (add1 d))
      ("bool" (if (equal d "t") "nil" "t"))
      (t (or (cadr (member d cs)) (car cs))))))

;; Measures one point: a fresh instance of master m in cv, vals (one per input of ins, nil:
;; its default) set in input order with the callbacks running as SfyImport runs them,
;; every netlisted param and every input read back, the instance deleted. Returns
;; (set-texts status values readbacks).
(procedure (SfyCharMeasure cv m ins outs vals)
  (let ((inst nil) (row nil) (set nil) (status nil) (icdf nil) (p nil) (got nil) (back nil) (v vals))
    (foreach i ins
      (when (car v) (setq row (cons (list (car i) (SfyCharText i (car v))) row)))
      (setq set (cons (if (car v) (SfyCharText i (car v)) "") set))
      (setq v (cdr v)))
    (setq row (reverse row))
    (setq inst (car (errset (schCreateInst cv m "SFYCHAR" (list 0.0 0.0) "R0") t)))
    (setq status (if inst (car (errset (SfySetParams inst row t) t)) "no instance"))
    (setq icdf (and inst (car (errset (cdfGetInstCDF inst) t))))
    (foreach n outs
      (setq p (and icdf (cdfFindParamByName icdf n)))
      (setq got (cons (if p (SfyAtomStr (getSGq p value)) "") got)))
    (foreach i ins
      (setq p (and icdf (cdfFindParamByName icdf (car i))))
      (setq back (cons (if p (SfyAtomStr (getSGq p value)) "") back)))
    (when inst (errset (dbDeleteObject inst) t))
    (list (reverse set) (if (equal status t) "ok" (sprintf nil "%L" status)) (reverse got) (reverse back))))

;; Measures and writes one point; t when every callback ran.
(procedure (SfyCharPoint port cv m ins outs vals)
  (let ((r (SfyCharMeasure cv m ins outs vals)))
    (SfyPut port "pt" r)
    (equal (cadr r) "ok")))

;; The inputs of a cell: the candidates (every CDF param with a callback, and every
;; netlisted param) whose nudge, from the defaults, changes some other netlisted param.
;; Writes one probe record per candidate (the names it changed, no values) to the list
;; probes (returned with the inputs as (inputs probes)).
(procedure (SfyCharDiscover cv m cdf outs names)
  (let ((cands nil) (c nil) (base nil) (r nil) (changed nil) (ins nil) (probes nil) (k 0))
    (foreach q (getSGq cdf parameters)
      (when (or (and (stringp (getSGq q callback)) (greaterp (strlen (getSGq q callback)) 0))
                (member (getSGq q name) outs) (member (getSGq q name) names))
        (setq c (SfyCharCand q))
        (when c (setq cands (cons c cands)))))
    (setq cands (reverse cands))
    (setq base (caddr (SfyCharMeasure cv m nil outs nil)))
    (foreach c cands
      (setq r (SfyCharMeasure cv m (list c) outs (list (SfyCharNudge c))))
      (setq changed nil)
      (setq k 0)
      (foreach n outs
        (when (and (nequal n (car c)) (nequal (nth k (caddr r)) (nth k base)))
          (setq changed (cons n changed)))
        (setq k (add1 k)))
      (setq changed (reverse changed))
      (setq probes (cons (list (car c) (cadr c) (car (car r)) (cadr r) changed) probes))
      (when (if names (member (car c) names) (and changed (equal (cadr r) "ok")))
        (setq ins (cons c ins))))
    (list (reverse ins) (reverse probes))))

;; The grid: every combination of the per-input value lists in lists (input order).
(procedure (SfyCharGrid port cv m ins outs lists acc)
  (if (null lists)
    (SfyCharPoint port cv m ins outs (reverse acc))
    (foreach v (car lists)
      (SfyCharGrid port cv m ins outs (cdr lists) (cons v acc)))))

;; The values an input takes on the grid: a number x1 x4 x16, a count 1 2 3 4, every
;; boolean and choice value.
(procedure (SfyCharLevels i)
  (case (cadr i)
    ("num" (list (caddr i) (times 4 (caddr i)) (times 16 (caddr i))))
    ("count" (list 1 2 3 4))
    ("bool" (list "t" "nil"))
    (t (cadddr i))))

;; Characterizes lib/cell into port (spec 8.3): discovery, then the grid and one fine table
;; per number input (the others at their defaults; from the default by default/20 up to
;; span times it). Given points (value lists in input order, nil for a default) replace
;; both: the holdout check of a cell's rules. t when the cell had something to sweep.
(procedure (SfyCharCell port cv lib cell names span points)
  (let ((m nil) (cdf nil) (outs nil) (found nil) (ins nil) (k 0) (step nil) (vals nil) (n 0))
    (setq m (car (errset (dbOpenCellViewByType lib cell "symbol" nil "r") t)))
    (setq cdf (car (errset (cdfGetCellCDF (ddGetObj lib cell)) t)))
    (setq outs (and cdf (cadr (assoc "instParameters"
                                (cadr (assoc "spectre" (SfySimInfoFields (getSGq cdf simInfo))))))))
    (setq found (and m cdf outs (SfyCharDiscover cv m cdf outs names)))
    (setq ins (car found))
    (cond
      ((not (and m cdf outs))
        (SfyRefuse 0 (sprintf nil "%s/%s: no symbol, CDF or spectre instParameters to characterize" lib cell))
        nil)
      (t
        (SfyPut port "char"
          (list lib cell (mapcar 'car ins) outs (mapcar 'cadr ins)
                (mapcar (lambda (i) (SfyCharText i (caddr i))) ins)))
        (foreach pr (cadr found) (SfyPut port "probe" pr))
        (cond
          ((null ins) nil)
          (points
            (foreach pt points
              (SfyCharPoint port cv m ins outs pt)))
          (t
            (SfyCharGrid port cv m ins outs (mapcar 'SfyCharLevels ins) nil)
            (foreach i ins
              (when (equal (cadr i) "num")
                (setq step (quotient (caddr i) 20.0))
                (setq k 0)
                (while (and (lessp k 2000) (lessp n 4000)
                            (leqp (times k step) (times (sub1 span) (caddr i))))
                  (setq vals (mapcar (lambda (j) (if (equal j i) (plus (caddr i) (times k step)) nil)) ins))
                  (SfyCharPoint port cv m ins outs vals)
                  (setq k (add1 k))
                  (setq n (add1 n)))))))
        (SfyCount "characterized")
        t))))

;; SfyCharacterize: characterizes cells (a list of names) of PDK library lib into path (an
;; .sfyx file; it names the PDK's parameters and values, so it stays on the machine it was
;; made on). scratch is a writable library for the throwaway schematic. ?inputs names the
;; inputs instead of discovering them; ?span sets how far each number input is swept
;; (default 100 times its default); ?points, value lists in input order, measures just
;; those points (the holdout check of a cell's rules). Writes <path>.log; t on PASS.
(procedure (SfyCharacterize path lib cells scratch @key (inputs nil) (span 100) (points nil))
  (let ((sfyLogPort (SfyLogOpen path)) (sfyCountTab (makeTable "sfyCounts" nil))
        (sfyCountKeys nil) (sfyFails 0) (port nil) (cv nil))
    (SfyLog (sprintf nil "SfyCharacterize %s library %s cells %L" path lib cells))
    (setq cv (car (errset (dbOpenCellViewByType scratch "sfy_char" "schematic" "schematic" "w") t)))
    (setq port (and cv (outfile path "w")))
    (cond
      ((null (ddGetObj lib)) (SfyRefuse 0 (sprintf nil "library %s is not in cds.lib" lib)))
      ((null cv) (SfyRefuse 0 (sprintf nil "cannot open %s/sfy_char/schematic for writing" scratch)))
      ((null port) (SfyRefuse 0 (sprintf nil "cannot write %s" path)))
      (t
        (unwindProtect
          (progn
            (SfyPut port "sfyx" (list 1 (SfyProducer)))
            (foreach c cells
              (unless (errset (SfyCharCell port cv lib c inputs span points) t)
                (SfyRefuse 0 (sprintf nil "%s/%s: SKILL error %s" lib c (SfyErrMsg)))))
            (SfyPut port "end" nil))
          (progn
            (close port)
            (errset (dbPurge cv) t)))))
    (SfyLogClose)))
