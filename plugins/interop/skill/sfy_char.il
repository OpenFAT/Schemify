;; sfy_char.il: SfyCharacterize, which measures what a PDK device's CDF callbacks derive from
;; the parameters a user sets (spec interop.md 8.3). The callbacks are SKILL, often
;; encrypted, and never cross to Schemify; their effect is measured here, on a scratch
;; schematic that is never saved, and written as .sfyx records:
;;   ("char" lib cell (input ...) (netlisted ...))
;;   ("pt" (set value per input, "" when not set) status (value per netlisted param))
;; Prefix form only, every Cadence call inside errset (the bridge's rules).

;; A CDF number string ("80n", "1.5u", "2") as a float; nil when it is not one.
(procedure (SfyCharNum s)
  (let ((n (and (stringp s) (strlen s))) (scale 1.0) (c nil))
    (when (and n (greaterp n 0))
      (setq c (substring s n 1))
      (setq scale (case c ("f" 1e-15) ("p" 1e-12) ("n" 1e-9) ("u" 1e-6) ("m" 1e-3)
                          ("k" 1e3) ("K" 1e3) ("M" 1e6) ("G" 1e9) (t 1.0)))
      (if (equal scale 1.0)
        (atof s)
        (and (greaterp n 1) (times (atof (substring s 1 (sub1 n))) scale))))))

;; One point: a fresh instance of master m in cv, the inputs set in the order of ins (w, l,
;; then the finger count: a callback may rewrite an earlier one) with the callbacks running
;; as SfyImport runs them, every netlisted param read back, the instance deleted.
(procedure (SfyCharPoint port cv m ins fing outs w l f)
  (let ((inst nil) (row nil) (set nil) (status nil) (icdf nil) (p nil) (vals nil))
    (when (and w (assoc "w" ins)) (setq row (cons (list "w" (sprintf nil "%g" w)) row)))
    (when (and l (assoc "l" ins)) (setq row (cons (list "l" (sprintf nil "%g" l)) row)))
    (when (and f fing) (setq row (cons (list fing (sprintf nil "%d" f)) row)))
    (setq row (reverse row))
    (foreach i ins
      (setq set (cons (or (cadr (assoc (car i) row)) "") set)))
    (setq inst (car (errset (schCreateInst cv m "SFYCHAR" (list 0.0 0.0) "R0") t)))
    (setq status (if inst (car (errset (SfySetParams inst row t) t)) "no instance"))
    (setq icdf (and inst (car (errset (cdfGetInstCDF inst) t))))
    (foreach n outs
      (setq p (and icdf (cdfFindParamByName icdf n)))
      (setq vals (cons (if p (SfyAtomStr (getSGq p value)) "") vals)))
    (SfyPut port "pt"
      (list (reverse set) (if (equal status t) "ok" (sprintf nil "%L" status)) (reverse vals)))
    (when inst (errset (dbDeleteObject inst) t))
    (equal status t)))

;; Characterizes lib/cell into port: the geometry grid (w x1 x4 x16, the finger count 1 2 3
;; 4, l x1 x2; each input only when the CDF has it editable with a numeric default) and the
;; length table (w and fingers at their defaults, l from its default by lstep up to lmax;
;; at most 4000 points). t when the cell had something to sweep.
(procedure (SfyCharCell port cv lib cell lstep lmax)
  (let ((m nil) (cdf nil) (outs nil) (ins nil) (p nil) (fing nil) (base nil) (ws nil)
        (ls nil) (nfs nil) (step nil) (top nil) (k 0) (w0 nil) (f0 nil))
    (setq m (car (errset (dbOpenCellViewByType lib cell "symbol" nil "r") t)))
    (setq cdf (car (errset (cdfGetCellCDF (ddGetObj lib cell)) t)))
    (setq outs (and cdf (cadr (assoc "instParameters"
                                (cadr (assoc "spectre" (SfySimInfoFields (getSGq cdf simInfo))))))))
    (cond
      ((not (and m cdf outs))
        (SfyRefuse 0 (sprintf nil "%s/%s: no symbol, CDF or spectre instParameters to characterize" lib cell))
        nil)
      (t
        (foreach n (list "w" "l" "nf" "nfin" "fingers")
          (setq p (cdfFindParamByName cdf n))
          (when (and p (nequal (getSGq p editable) "nil") (SfyCharNum (SfyAtomStr (getSGq p defValue))))
            (setq ins (cons (list n (SfyCharNum (SfyAtomStr (getSGq p defValue)))) ins))))
        (setq ins (reverse ins))
        (SfyPut port "char" (list lib cell (mapcar 'car ins) outs))
        (setq fing (car (setof n (list "nf" "nfin" "fingers") (assoc n ins))))
        (setq w0 (cadr (assoc "w" ins)))
        (setq f0 (and fing (round (cadr (assoc fing ins)))))
        (setq ws (if w0 (list w0 (times 4 w0) (times 16 w0)) '(nil)))
        (setq base (cadr (assoc "l" ins)))
        (setq ls (if base (list base (times 2 base)) '(nil)))
        (setq nfs (if fing '(1 2 3 4) '(nil)))
        (foreach w ws
          (foreach l ls
            (foreach f nfs
              (SfyCharPoint port cv m ins fing outs w l f))))
        (when base
          (setq step (if lstep (SfyCharNum lstep) (quotient base 20.0)))
          (setq top (if lmax (SfyCharNum lmax) (times 100 base)))
          (while (and (lessp k 4000) (leqp (plus base (times k step)) (plus top (times 0.5 step))))
            (SfyCharPoint port cv m ins fing outs w0 (plus base (times k step)) f0)
            (setq k (add1 k))))
        (SfyCount "characterized")
        t))))

;; SfyCharacterize: characterizes cells (a list of names) of PDK library lib into path (an
;; .sfyx file; it names the PDK's parameters and values, so it stays on the machine it was
;; made on). scratch is a writable library for the throwaway schematic. ?lstep and ?lmax
;; (CDF number strings) set the length table. Writes <path>.log; t on PASS.
(procedure (SfyCharacterize path lib cells scratch @key (lstep nil) (lmax nil))
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
              (unless (errset (SfyCharCell port cv lib c lstep lmax) t)
                (SfyRefuse 0 (sprintf nil "%s/%s: SKILL error %s" lib c (SfyErrMsg)))))
            (SfyPut port "end" nil))
          (progn
            (close port)
            (errset (dbPurge cv) t)))))
    (SfyLogClose)))
