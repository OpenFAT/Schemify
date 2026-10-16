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
    ("num" (sprintf nil "%.9g" v))
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

;; Per-cell sweep state: the port, cellview, master, inputs, netlisted names, and the
;; points left in the budget (spec 8.3: at most 4000 a device).
(procedure (SfyCharBudget)
  (greaterp sfyCharLeft 0))

;; Measures and writes one point within the budget; the measure, or nil past the budget.
(procedure (SfyCharTake vals)
  (let ((r nil))
    (when (SfyCharBudget)
      (setq sfyCharLeft (sub1 sfyCharLeft))
      (setq r (SfyCharMeasure sfyCharCv sfyCharM sfyCharIns sfyCharOuts vals))
      (SfyPut sfyCharPort "pt" r))
    r))

;; What a point says about input i (its index k): whether it read back as set, and the
;; netlisted values mask marks (the ones that step along this axis). Two points with the
;; same signature need no point between them.
(procedure (SfyCharSig r i k mask)
  (let ((set (SfyCharNum (nth k (car r)))) (back (SfyCharNum (nth k (cadddr r)))) (vals nil) (n 0))
    (foreach o sfyCharOuts
      (when (and (nth n mask) (nequal o (car i))) (setq vals (cons (nth n (caddr r)) vals)))
      (setq n (add1 n)))
    (list (cadr r)
          (and set back (lessp (abs (difference set back)) (times 1e-6 (max (abs set) 1e-30))))
          vals)))

;; The value list with input k at v, the others at their defaults.
(procedure (SfyCharAt k v)
  (let ((out nil) (n 0))
    (foreach j sfyCharIns
      (setq out (cons (if (equal n k) v nil) out))
      (setq n (add1 n)))
    (reverse out)))

;; Bisects number input i (index k) between a and b, whose signatures differ, down to a
;; relative width of 1e-5 (the set values carry 9 digits): finds a table's breakpoint or
;; where the callbacks start rewriting the input.
(procedure (SfyCharBisect i k mask a sa b sb depth)
  ;; A work list of (a sa b sb depth), not recursion: the depth stays flat.
  (let ((work (list (list a sa b sb depth))) (w nil) (m nil) (r nil) (sm nil))
    (while (and work (SfyCharBudget))
      (setq w (car work))
      (setq work (cdr work))
      (setq a (nth 0 w))
      (setq sa (nth 1 w))
      (setq b (nth 2 w))
      (setq sb (nth 3 w))
      (setq depth (nth 4 w))
      (when (and (lessp depth 40)
                 (greaterp (difference b a) (times 1e-5 (max (abs b) 1e-30))))
        (setq m (quotient (plus a b) 2.0))
        (setq r (SfyCharTake (SfyCharAt k m)))
        (when r
          (setq sm (SfyCharSig r i k mask))
          (unless (equal sm sb) (setq work (cons (list m sm b sb (add1 depth)) work)))
          (unless (equal sm sa) (setq work (cons (list a sa m sm (add1 depth)) work))))))))

;; Which netlisted values step along an axis: those that change between at most a third
;; of the neighbouring scan points (scan, a list of measures by increasing value). A value
;; that changes everywhere varies continuously: bisecting it would split every interval.
(procedure (SfyCharSteps scan)
  (let ((mask nil) (pairs (max 1 (sub1 (length scan)))) (n 0) (c 0) (prev nil))
    (foreach o sfyCharOuts
      (setq c 0)
      (setq prev nil)
      (foreach r scan
        (when (and prev (nequal (nth n (caddr r)) (nth n (caddr prev)))) (setq c (add1 c)))
        (setq prev r))
      (setq mask (cons (leqp (times 3 c) pairs) mask))
      (setq n (add1 n)))
    (reverse mask)))

;; One axis: number input i scanned from a quarter of its default up to span times it by
;; steps of x1.2, then bisected wherever a stepping value or the read-back changes; a
;; count at 1 2 3 4 8; every boolean and choice value. The others stay at their defaults.
(procedure (SfyCharAxis i k span)
  (let ((d (caddr i)) (x nil) (xs nil) (scan nil) (r nil) (mask nil) (prev nil) (ps nil) (s nil))
    (case (cadr i)
      ("num"
        (setq x (if (zerop d) 1e-9 (quotient d 4.0)))
        ;; The scan ends on span times the default itself: random points reach it.
        (while (and (SfyCharBudget) x)
          (setq r (SfyCharTake (SfyCharAt k x)))
          (when r
            (setq xs (cons x xs))
            (setq scan (cons r scan)))
          (setq x (cond ((equal x (times span (if (zerop d) 1e-9 d))) nil)
                        ((leqp (times x 1.2) (times span (if (zerop d) 1e-9 d))) (times x 1.2))
                        (t (times span (if (zerop d) 1e-9 d))))))
        (setq xs (reverse xs))
        (setq scan (reverse scan))
        (setq mask (SfyCharSteps scan))
        (foreach r scan
          (setq x (car xs))
          (setq xs (cdr xs))
          (setq s (SfyCharSig r i k mask))
          (when (and prev (nequal s ps)) (SfyCharBisect i k mask prev ps x s 0))
          (setq prev x)
          (setq ps s)))
      ("count" (foreach v (list 1 2 3 4 8) (SfyCharTake (SfyCharAt k v))))
      ("bool" (foreach v (list "t" "nil") (SfyCharTake (SfyCharAt k v))))
      (t (foreach v (cadddr i) (SfyCharTake (SfyCharAt k v)))))))

;; Two values of input i away from its default, for the cross points.
(procedure (SfyCharOff i)
  (let ((d (caddr i)))
    (case (cadr i)
      ("num" (if (zerop d) (list 1e-6 1e-5) (list (times 4 d) (times 10 d))))
      ("count" (list 2 4))
      ("bool" (list (if (equal d "t") "nil" "t")))
      (t (setof c (cadddr i) (nequal c d))))))

;; A seeded pseudo-random number in [0, 1): the same cross points every run.
(procedure (SfyCharRand)
  ;; Small constants: SKILL integers may be 32-bit.
  (setq sfyCharSeed (mod (plus (times sfyCharSeed 421) 54773) 259200))
  (quotient (float sfyCharSeed) 259200.0))

;; A random value of input i: a number among its axis's scan values, a count from 1 to
;; 8, a random boolean or choice.
(procedure (SfyCharAny i span)
  (let ((d (caddr i)) (x nil) (k 0))
    (case (cadr i)
      ("num"
        (setq x (if (zerop d) 1e-9 (quotient d 4.0)))
        (setq k (fix (times (SfyCharRand) 34)))
        (while (greaterp k 0) (setq x (times x 1.2)) (setq k (sub1 k)))
        (min x (times span (if (zerop d) 1e-9 d))))
      ("count" (add1 (fix (times (SfyCharRand) 8))))
      ("bool" (if (lessp (SfyCharRand) 0.5) "t" "nil"))
      (t (nth (fix (times (SfyCharRand) (length (cadddr i)))) (cadddr i))))))

;; The cross points: every pair of inputs at their off values (the others at defaults),
;; then ten random points. A rule fitted from the axes must reproduce them all.
(procedure (SfyCharCross span)
  (let ((n (length sfyCharIns)))
    (for a 0 (sub1 n)
      (for b (add1 a) (sub1 n)
        (foreach va (SfyCharOff (nth a sfyCharIns))
          (foreach vb (SfyCharOff (nth b sfyCharIns))
            (SfyCharTake (SfyCharTake1 (SfyCharAt a va) b vb))))))
    (setq sfyCharSeed 1015)
    (for r 1 10
      (SfyCharTake (mapcar (lambda (i) (SfyCharAny i span)) sfyCharIns)))))

;; vals with input k set to v.
(procedure (SfyCharTake1 vals k v)
  (let ((out nil) (n 0))
    (foreach x vals
      (setq out (cons (if (equal n k) v x) out))
      (setq n (add1 n)))
    (reverse out)))

;; Characterizes lib/cell into port (spec 8.3): discovery, the defaults, one axis per
;; input with bisection, the cross points. Given points (value lists in input order, nil
;; for a default) replace the sweep: the holdout check of a cell's rules. t when the cell
;; had something to sweep.
(procedure (SfyCharCell port cv lib cell names span points)
  (let ((m nil) (cdf nil) (outs nil) (found nil) (ins nil))
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
            (let ((sfyCharPort port) (sfyCharCv cv) (sfyCharM m) (sfyCharIns ins)
                  (sfyCharOuts outs) (sfyCharLeft 4000) (sfyCharSeed 1015) (k 0))
              (SfyCharTake (mapcar (lambda (i) nil) ins))
              (foreach i ins
                (SfyCharAxis i k span)
                (setq k (add1 k)))
              (SfyCharCross span)
              (unless (SfyCharBudget)
                (SfyLog (sprintf nil "info %s/%s: the 4000-point budget ran out" lib cell))))))
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
