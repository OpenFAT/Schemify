;; sfy_retarget.il (interop spec section 10): gm/Id retarget on the target machine.
;;
;; Pure math first (no database or simulator calls): table construction, inverse lookup,
;; interpolation, solve, finger split and L snap. It mirrors src/retarget.rs operation for
;; operation, so the two agree to the last bits; tests/retarget.rs runs it in the test
;; interpreter against the Rust reference on random vectors.
;;
;; A table is (vdsNom lAxis vdsAxis vgsAxis rows), rows = one list per L of one list per
;; VDS of one (id gm gds cgg) list per VGS point, all per metre of width, axes ascending.

(procedure (SfyRtLerp a b frac)
  (plus a (times frac (difference b a))))

;; Geometric interpolation of positive quantities; linear when a sign is off.
(procedure (SfyRtGeo a b frac)
  (if (and (greaterp a 0.0) (greaterp b 0.0))
    (times a (expt (quotient b a) frac))
    (SfyRtLerp a b frac)))

;; gm/Id of (id gm gds cgg); 0.0 off the usable branch.
(procedure (SfyRtGmId q)
  (if (and (greaterp (car q) 0.0) (greaterp (cadr q) 0.0)
           (greaterp (caddr q) 0.0) (greaterp (cadddr q) 0.0))
    (quotient (cadr q) (car q))
    0.0))

;; Number of axis entries below x (Rust partition_point).
(procedure (SfyRtCountBelow axis x)
  (let ((k 0))
    (while (and axis (lessp (car axis) x))
      (setq k (add1 k))
      (setq axis (cdr axis)))
    k))

;; Table from sweeps at width w: samples nested like rows, absolute values.
(procedure (SfyRtTableMake vdsNom lAxis vdsAxis vgsAxis w samples)
  (list vdsNom lAxis vdsAxis vgsAxis
    (mapcar
      (lambda (rowL)
        (mapcar
          (lambda (rowD)
            (mapcar
              (lambda (q) (mapcar (lambda (x) (quotient (abs x) w)) q))
              rowD))
          rowL))
      samples)))

;; (d0 d1 frac) for VDS v, or nil outside the axis (1 nV slack).
(procedure (SfyRtVdsBracket axis v)
  (prog (vFirst vLast d1 n vd1)
    (setq vFirst (car axis))
    (setq vLast (car (last axis)))
    (setq n (length axis))
    (unless (and (geqp v (difference vFirst 1.0e-9)) (leqp v (plus vLast 1.0e-9)))
      (return nil))
    (setq v (max vFirst (min v vLast)))
    (setq d1 (min (SfyRtCountBelow axis v) (sub1 n)))
    (setq vd1 (nth d1 axis))
    (when (or (zerop d1) (equal vd1 v))
      (return (list d1 d1 0.0)))
    (return (list (sub1 d1) d1
      (quotient (difference v (nth (sub1 d1) axis)) (difference vd1 (nth (sub1 d1) axis)))))))

;; Two VDS rows blended point by point: (id gm gds cgg) per VGS point.
(procedure (SfyRtBlend r0 r1 frac)
  (let ((out nil) (a nil) (b nil))
    (while r0
      (setq a (car r0))
      (setq b (car r1))
      (setq out (cons
        (list (SfyRtLerp (car a) (car b) frac) (SfyRtLerp (cadr a) (cadr b) frac)
              (SfyRtLerp (caddr a) (caddr b) frac) (SfyRtLerp (cadddr a) (cadddr b) frac))
        out))
      (setq r0 (cdr r0))
      (setq r1 (cdr r1)))
    (reverse out)))

;; Point (vgs jd gm gds cgg) on L row li, or nil: scan down from the highest VGS to the
;; first point whose gm/Id reaches gmid (dips crossed, cutoff ends the row).
(procedure (SfyRtRowPoint tab li gmid d)
  (prog (rowL q vg hi hiV lo loV vHi vLo frac)
    (setq rowL (nth li (nth 4 tab)))
    (setq q (reverse (SfyRtBlend (nth (car d) rowL) (nth (cadr d) rowL) (caddr d))))
    (setq vg (reverse (nth 3 tab)))
    (setq hi (car q))
    (setq vHi (car vg))
    (setq hiV (SfyRtGmId hi))
    (when (or (leqp hiV 0.0) (lessp gmid hiV))
      (return nil))
    (setq q (cdr q))
    (setq vg (cdr vg))
    (while q
      (setq lo (car q))
      (setq vLo (car vg))
      (setq loV (SfyRtGmId lo))
      (when (leqp loV 0.0)
        (return nil))
      (when (leqp gmid loV)
        (setq frac (if (greaterp loV hiV) (quotient (difference gmid hiV) (difference loV hiV)) 0.0))
        (return (list (SfyRtLerp vHi vLo frac)
          (SfyRtGeo (car hi) (car lo) frac) (SfyRtGeo (cadr hi) (cadr lo) frac)
          (SfyRtGeo (caddr hi) (caddr lo) frac) (SfyRtGeo (cadddr hi) (cadddr lo) frac))))
      (setq hi lo)
      (setq hiV loV)
      (setq vHi vLo)
      (setq q (cdr q))
      (setq vg (cdr vg)))
    (return nil)))

;; Point (vgs jd gm gds cgg) at (gmid, l, vds), vds nil = the table's nominal VDS; nil
;; when unreachable or off the axes. L rows blend in log L.
(procedure (SfyRtPoint tab gmid l vds)
  (prog (lax d lFirst lLast i1 l0 l1 p0 p1 frac)
    (setq lax (cadr tab))
    (unless (and (greaterp gmid 0.0) (greaterp l 0.0))
      (return nil))
    (setq d (SfyRtVdsBracket (caddr tab) (if vds vds (car tab))))
    (unless d
      (return nil))
    (setq lFirst (car lax))
    (setq lLast (car (last lax)))
    (when (or (lessp l (times lFirst (difference 1.0 1.0e-9))) (greaterp l (times lLast (plus 1.0 1.0e-9))))
      (return nil))
    (setq l (max lFirst (min l lLast)))
    (setq i1 (SfyRtCountBelow lax l))
    (setq l1 (nth i1 lax))
    (when (or (zerop i1) (lessp (difference 1.0 (quotient l l1)) 1.0e-9))
      (return (SfyRtRowPoint tab i1 gmid d)))
    (setq l0 (nth (sub1 i1) lax))
    (when (lessp (difference (quotient l l0) 1.0) 1.0e-9)
      (return (SfyRtRowPoint tab (sub1 i1) gmid d)))
    (setq p0 (SfyRtRowPoint tab (sub1 i1) gmid d))
    (setq p1 (SfyRtRowPoint tab i1 gmid d))
    (unless (and p0 p1)
      (return nil))
    (setq frac (quotient (log (quotient l l0)) (log (quotient l1 l0))))
    (return (list (SfyRtLerp (car p0) (car p1) frac)
      (SfyRtGeo (nth 1 p0) (nth 1 p1) frac) (SfyRtGeo (nth 2 p0) (nth 2 p1) frac)
      (SfyRtGeo (nth 3 p0) (nth 3 p1) frac) (SfyRtGeo (nth 4 p0) (nth 4 p1) frac)))))

;; (vgs jd w residual) for one intent, or nil; w = id / jd, residual = gm/Id - gmid.
(procedure (SfyRtSolve tab gmid l id vds)
  (let ((p nil))
    (when (greaterp id 0.0)
      (setq p (SfyRtPoint tab gmid l vds)))
    (when p
      (list (car p) (cadr p) (quotient id (cadr p))
        (difference (quotient (caddr p) (cadr p)) gmid)))))

;; L rounded to the nearest multiple of lgrid, at least one step; lgrid nil keeps L.
(procedure (SfyRtSnapL l lgrid)
  (if (and lgrid (greaterp lgrid 0.0))
    (times (max 1 (round (quotient l lgrid))) lgrid)
    l))

;; Fingers for total width w: nf, raised to ceil(w / fingerwMax) (1e-9 slack).
(procedure (SfyRtFingers w nf fingerwMax)
  (let ((q 0.0) (k 0))
    (if (and fingerwMax (greaterp fingerwMax 0.0))
      (progn
        (setq q (times (quotient w fingerwMax) (difference 1.0 1.0e-9)))
        (setq k (fix q))
        (when (greaterp q k)
          (setq k (add1 k)))
        (max nf k))
      nf)))

;; The finger rule of the target map (Rust retarget::finger_rule): SfyRtFingers from one
;; finger, then split_above (never split at or below it, at least two above) or the
;; intent's nf, capped so each finger stays at least fingerwMin wide unless fingerwMax
;; needs more. nil arguments are absent attributes.
(procedure (SfyRtFingerRule w nf fingerwMax fingerwMin splitAbove)
  (let ((lo 1) (base 1) (hi 4096))
    (setq lo (SfyRtFingers w 1 fingerwMax))
    (cond
      ((and splitAbove (leqp w (times splitAbove (plus 1.0 1.0e-9)))) (setq base 1))
      (splitAbove (setq base (max lo 2)))
      (t (setq base (max nf lo))))
    (when fingerwMin
      (setq hi (max 1 (fix (times (quotient w fingerwMin) (plus 1.0 1.0e-9))))))
    (max lo (min base hi))))

;; True when a and b agree within rel relative (both nil also agrees).
(procedure (SfyRtClose a b rel)
  (cond
    ((and (null a) (null b)) t)
    ((or (null a) (null b)) nil)
    (t (leqp (abs (difference a b)) (times rel (max (abs a) (abs b)))))))

;; Runs the built-in vectors (SfyRtSelfData, generated from the Rust reference) and prints
;; PASS or FAIL with the first failures; returns t on PASS.
(procedure (SfyRetargetSelfTest)
  (let ((data (SfyRtSelfData)) (tab nil) (fails 0) (n 0) (got nil) (want nil) (ok nil))
    (setq tab (SfyRtTableMake (nth 0 data) (nth 1 data) (nth 2 data) (nth 3 data) (nth 4 data) (nth 5 data)))
    (foreach v (nth 6 data)
      (setq n (add1 n))
      (setq got (SfyRtSolve tab (nth 0 v) (nth 1 v) (nth 3 v) (nth 2 v)))
      (setq want (nth 4 v))
      (setq ok (if (and got want)
                 (and (SfyRtClose (nth 0 got) (nth 0 want) 1.0e-9)
                      (SfyRtClose (nth 1 got) (nth 1 want) 1.0e-9)
                      (SfyRtClose (nth 2 got) (nth 2 want) 1.0e-9)
                      (leqp (abs (difference (nth 3 got) (nth 3 want))) 1.0e-9))
                 (and (null got) (null want))))
      (unless ok
        (setq fails (add1 fails))
        (when (leqp fails 5)
          (printf "SfyRetargetSelfTest: vector %d: got %L want %L\n" n got want))))
    (foreach v (nth 7 data)
      (setq n (add1 n))
      (unless (SfyRtClose (SfyRtSnapL (nth 0 v) (nth 1 v)) (nth 2 v) 1.0e-12)
        (setq fails (add1 fails))
        (printf "SfyRetargetSelfTest: snap %d: %L\n" n v)))
    (foreach v (nth 8 data)
      (setq n (add1 n))
      (unless (equal (SfyRtFingers (nth 0 v) (nth 1 v) (nth 2 v)) (nth 3 v))
        (setq fails (add1 fails))
        (printf "SfyRetargetSelfTest: fingers %d: %L\n" n v)))
    (if (zerop fails)
      (progn (printf "SfyRetargetSelfTest: PASS (%d vectors)\n" n) t)
      (progn (printf "SfyRetargetSelfTest: FAIL (%d of %d vectors)\n" fails n) nil))))

;; ---------------------------------------------------------------------------------------
;; Target map (.retarget, line records, spec section 10.2)
;; ---------------------------------------------------------------------------------------

;; Words and quoted values of one line (format spec section 2: escapes \" \\ \n \t;
;; ponytail: \u{...} is not decoded, no class line needs it).
(procedure (SfyRtWords line)
  (let ((out nil) (n (strlen line)) (i 1) (c nil) (tok nil) (inq nil))
    (while (leqp i n)
      (setq c (substring line i 1))
      (cond
        (inq
          (cond
            ((and (equal c "\\") (lessp i n))
              (setq i (add1 i))
              (setq c (substring line i 1))
              (setq tok (strcat tok (case c ("n" "\n") ("t" "\t") (t c)))))
            ((equal c "\"")
              (setq out (cons tok out))
              (setq tok nil)
              (setq inq nil))
            (t (setq tok (strcat tok c)))))
        ((equal c "\"")
          (setq inq t)
          (setq tok ""))
        ((member c (list " " "\t" "\r" "\n"))
          (when tok
            (setq out (cons tok out))
            (setq tok nil)))
        (t (setq tok (if tok (strcat tok c) c))))
      (setq i (add1 i)))
    (when tok
      (setq out (cons tok out)))
    (reverse out)))

;; A target-map number: decimal times one suffix factor (f p n u m k meg g t), nil if not.
(procedure (SfyRtNum s)
  (let ((low (lowerCase s)) (n (strlen s)) (scale 1.0) (body s) (v nil) (suf nil))
    (setq suf (assoc (substring low n 1)
      (list (list "f" 1.0e-15) (list "p" 1.0e-12) (list "n" 1.0e-9) (list "u" 1.0e-6)
            (list "m" 1.0e-3) (list "k" 1000.0) (list "g" 1.0e9) (list "t" 1.0e12))))
    (cond
      ((and (greaterp n 3) (equal (substring low (difference n 2) 3) "meg"))
        (setq scale 1.0e6)
        (setq body (substring s 1 (difference n 3))))
      ((and suf (greaterp n 1))
        (setq scale (cadr suf))
        (setq body (substring s 1 (sub1 n)))))
    (when (member (substring body 1 1) (list "0" "1" "2" "3" "4" "5" "6" "7" "8" "9"))
      (setq v (atof body t)))
    (when (and v (greaterp v 0.0))
      (times v scale))))

;; Reads a .retarget file: a list of classes, each (header attrRow...), header =
;; ("class" name "lib" lib "cell" cell), attrRow = (key value...), numbers still text.
;; corner records (the export's --target, spec section 10.2) are skipped here.
;; Returns (classes) or (nil "line N: reason").
(procedure (SfyRtReadMap path)
  (prog (port line words classes cur ln incorner)
    (setq port (infile path))
    (unless port
      (return (list nil (strcat "cannot read " path))))
    (setq ln 0)
    (while (gets line port)
      (setq ln (add1 ln))
      (setq words (SfyRtWords line))
      (cond
        ((or (null words) (equal (substring (car words) 1 1) "#")) nil)
        ((and incorner (member (substring line 1 1) (list " " "\t"))) nil)
        ((member (substring line 1 1) (list " " "\t"))
          (unless cur
            (close port)
            (return (list nil (sprintf nil "line %d: attribute outside a class" ln))))
          (setq cur (append1 cur words)))
        ((and (equal (car words) "corner") (equal (length words) 2))
          (when cur
            (setq classes (cons cur classes)))
          (setq cur nil)
          (setq incorner t))
        ((and (equal (car words) "class") (equal (length words) 6)
              (equal (nth 2 words) "lib") (equal (nth 4 words) "cell"))
          (when cur
            (setq classes (cons cur classes)))
          (setq incorner nil)
          (setq cur (list words)))
        (t
          (close port)
          (return (list nil (sprintf nil "line %d: expected class <name> lib <lib> cell <cell>" ln))))))
    (close port)
    (when cur
      (setq classes (cons cur classes)))
    (foreach c classes
      (foreach k (list "model" "device" "vdd")
        (unless (SfyRtAttr c k)
          (return (list nil (sprintf nil "class %s: missing %s" (nth 1 (car c)) k)))))
      (foreach k (list "w" "l")
        (unless (SfyRtMapName c "param" k)
          (return (list nil (sprintf nil "class %s: missing param %s" (nth 1 (car c)) k)))))
      (unless (SfyRtNum (SfyRtAttr c "vdd"))
        (return (list nil (sprintf nil "class %s: vdd is not a positive number" (nth 1 (car c)))))))
    (return (list (reverse classes)))))

;; First value of attribute key of class cls, or nil.
(procedure (SfyRtAttr cls key)
  (cadr (assoc key (cdr cls))))

;; Target name of class pin / param name (rows (pin a b), (param a b)), or nil.
(procedure (SfyRtMapName cls kind name)
  (let ((row nil))
    (foreach r (cdr cls)
      (when (and (equal (car r) kind) (equal (cadr r) name))
        (setq row r)))
    (and row (caddr row))))

;; ---------------------------------------------------------------------------------------
;; Target procedure (spec section 10.3)
;; ---------------------------------------------------------------------------------------

;; Contract with sfy_cells.il (the SKILL-cells part provides these; TODO until it lands):
;;   (SfySetParams inst params callbacks) -> t/nil
;;     inst: a schematic instance; params: list of (cdfName value) where value is a float
;;     (w, l in metres), an integer (nf) or a string (a fixed size's ADE expression; int
;;     and float CDF parameters read it with atoi/atof). Sets each through the instance CDF with the
;;     CDF's own type and full precision (never a 7-digit print), then, when callbacks is
;;     non-nil, runs the CDF callbacks of the set params in CDF order inside errset and logs
;;     what the PDK clamped (spec section 5.3).
;;   (SfyPlaceConn cv inst conns) -> number of stubs drawn, or nil
;;     conns: list of (terminal net), terminal a pin of inst's master. Draws one grid-step
;;     stub away from the instance at each terminal's real pin and glues a wire label with
;;     the net (spec section 6.4 step 2, the loader half of the conn records).

;; One intent of the .sfyx, joined with its instance: (lib cell view upi inst conns intent),
;; inst = ("inst" ref name lib cell view x y orient), conns = list of (term net),
;; intent = ("intent" ref class ((key value)...)).
(procedure (SfyRtReadIntents path)
  (prog (port rec lib cell view upi insts conns out)
    (setq port (infile path))
    (unless port
      (return nil))
    (while (setq rec (lineread port))
      (when (listp rec)
        (setq rec (car rec))
        (case (car rec)
          ("lib" (setq lib (cadr rec)))
          ("cell" (setq cell (cadr rec)))
          ("cv"
            (setq view (nth 1 rec))
            (setq upi (nth 3 rec))
            (setq insts nil)
            (setq conns nil))
          ("inst" (setq insts (cons (list (nth 1 rec) rec) insts)))
          ("conn" (setq conns (cons (list (nth 1 rec) (list (nth 2 rec) (nth 3 rec))) conns)))
          ("intent"
            (setq out (cons
              (list lib cell view upi (cadr (assoc (nth 1 rec) insts))
                (mapcar 'cadr (setof c (reverse conns) (equal (car c) (nth 1 rec))))
                rec)
              out))))))
    (close port)
    (return (reverse out))))

;; Intent field key as a float, or nil.
(procedure (SfyRtField intent key)
  (let ((kv (assoc key (nth 3 intent))))
    (and kv (atof (cadr kv) t))))

;; Sorted list without repeats.
(procedure (SfyRtUniq xs)
  (let ((out nil))
    (foreach x (sort xs 'lessp)
      (unless (and out (equal (car out) x))
        (setq out (cons x out))))
    (reverse out)))

;; Characterizes class cls with Spectre through OCEAN at lengths ls and drain voltages vdss
;; (both ascending). Returns a table, or a string naming the failure. Writes the netlist
;; (dir)/netlist with netlistHeader/netlistFooter beside it (OCEAN's direct-simulator
;; layout). ponytail: one run per (L, VDS); a paramAnalysis would batch them.
(procedure (SfyRtCharacterize cls ls vdss dir)
  (prog (vdd sign op port wname lname nfname steps rows rowL rowD ws vg)
    (setq vdd (SfyRtNum (SfyRtAttr cls "vdd")))
    (setq sign (if (equal (SfyRtAttr cls "polarity") "p") -1.0 1.0))
    (setq op (or (SfyRtAttr cls "op") (car (parseString (SfyRtAttr cls "device")))))
    (setq wname (SfyRtMapName cls "param" "w"))
    (setq lname (SfyRtMapName cls "param" "l"))
    (setq nfname (SfyRtMapName cls "param" "nf"))
    (setq steps 72)
    (createDir dir)
    (setq port (outfile (strcat dir "/netlistHeader") "w"))
    (fprintf port "simulator lang=spectre\n")
    (close port)
    (setq port (outfile (strcat dir "/netlistFooter") "w"))
    (close port)
    (setq port (outfile (strcat dir "/netlist") "w"))
    (fprintf port "%s\n" (SfyRtAttr cls "model"))
    (fprintf port "VG (g 0) vsource dc=vgs\nVD (d 0) vsource dc=vds\n")
    (fprintf port "VS (s 0) vsource dc=0\nVB (b 0) vsource dc=0\n")
    (fprintf port "%s %s=1u %s=lch%s\n" (SfyRtAttr cls "device") wname lname
      (if nfname (strcat " " nfname "=1") ""))
    (close port)
    (unless (simulator 'spectre)
      (return "simulator spectre is not available"))
    (unless (design (strcat dir "/netlist"))
      (return "design: the netlist was refused"))
    (resultsDir (strcat dir "/psf"))
    (save 'i (strcat op ":ids") (strcat op ":gm") (strcat op ":gds") (strcat op ":cgg"))
    (if (greaterp sign 0.0)
      (analysis 'dc ?param "vgs" ?start 0.0 ?stop vdd ?step (quotient vdd steps))
      (analysis 'dc ?param "vgs" ?start (minus vdd) ?stop 0.0 ?step (quotient vdd steps)))
    (foreach l ls
      (setq rowL nil)
      (foreach v vdss
        (desVar "vgs" 0.0 "vds" (times sign v) "lch" l)
        (unless (run)
          (return (sprintf nil "run failed at L %L VDS %L" l v)))
        (setq ws (list (getData (strcat op ":ids") ?result 'dc) (getData (strcat op ":gm") ?result 'dc)
                       (getData (strcat op ":gds") ?result 'dc) (getData (strcat op ":cgg") ?result 'dc)))
        (unless (and (car ws) (cadr ws) (caddr ws) (cadddr ws))
          (return (sprintf nil "no operating point data at L %L VDS %L" l v)))
        (setq rowD nil)
        (for k 0 steps
          (setq vg (times sign (quotient (times k vdd) (float steps))))
          (setq rowD (cons (mapcar (lambda (wave) (value wave vg)) ws) rowD)))
        (setq rowL (cons (reverse rowD) rowL)))
      (setq rows (cons (reverse rowL) rows)))
    (setq vg nil)
    (for k 0 steps
      (setq vg (cons (quotient (times k vdd) (float steps)) vg)))
    (return (SfyRtTableMake (quotient vdd 2.0) ls vdss (reverse vg) 1.0e-6 (reverse rows)))))

;; The (cdfName value) list of a solved size: w (per finger unless the map's width is
;; total) and l in metres, nf when the class maps it.
(procedure (SfyRtSizeParams cls w nf l)
  (let ((params nil))
    (setq params (list (list (SfyRtMapName cls "param" "w")
                             (if (equal (SfyRtAttr cls "width") "total") w (quotient w nf)))
                       (list (SfyRtMapName cls "param" "l") l)))
    (when (SfyRtMapName cls "param" "nf")
      (setq params (append1 params (list (SfyRtMapName cls "param" "nf") nf))))
    params))

;; The (cdfName value) list of a fixed-size placeholder (spec section 10.5): its values are
;; ADE expressions (strings, so a swept design variable stays one); w over nf for a
;; finger-width map; nf and m other than "1" need a mapped param. A string names a failure.
(procedure (SfyRtFixedParams cls intent)
  (prog (w l nf m params)
    (setq w (cadr (assoc "w" (nth 3 intent))))
    (setq l (cadr (assoc "l" (nth 3 intent))))
    (setq nf (cadr (assoc "nf" (nth 3 intent))))
    (setq m (cadr (assoc "m" (nth 3 intent))))
    (unless (and w l nf m)
      (return "a fixed size needs w, l, nf and m"))
    (when (and (nequal nf "1") (null (SfyRtMapName cls "param" "nf")))
      (return (strcat "nf " nf " but the class maps no param nf")))
    (when (and (nequal m "1") (null (SfyRtMapName cls "param" "m")))
      (return (strcat "m " m " but the class maps no param m")))
    (setq params (list (list (SfyRtMapName cls "param" "w")
                             (if (or (equal nf "1") (equal (SfyRtAttr cls "width") "total"))
                               w
                               (strcat "(" w ")/(" nf ")")))
                       (list (SfyRtMapName cls "param" "l") l)))
    (when (SfyRtMapName cls "param" "nf")
      (setq params (append1 params (list (SfyRtMapName cls "param" "nf") nf))))
    (when (SfyRtMapName cls "param" "m")
      (setq params (append1 params (list (SfyRtMapName cls "param" "m") m))))
    (return params)))

;; Places one intent e (see SfyRtReadIntents) as an instance of class cls with the
;; (cdfName value) list params. Returns t, or a string naming the failure.
(procedure (SfyRtPlace e cls params callbacks)
  (prog (inst cv master obj upi conns)
    (setq inst (nth 4 e))
    (setq upi (float (nth 3 e)))
    (setq cv (dbOpenCellViewByType (nth 0 e) (nth 1 e) (nth 2 e) nil "a"))
    (unless cv
      (return "cannot open the cellview (run SfyImport on the same file first)"))
    (setq master (dbOpenCellViewByType (nth 3 (car cls)) (nth 5 (car cls)) "symbol" nil "r"))
    (unless master
      (return "cannot open the class master symbol"))
    (setq obj (schCreateInst cv master (nth 2 inst)
      (list (quotient (float (nth 6 inst)) upi) (quotient (float (nth 7 inst)) upi)) (nth 8 inst)))
    (unless obj
      (return "schCreateInst failed"))
    (unless (SfySetParams obj params callbacks)
      (return "SfySetParams failed"))
    (setq conns nil)
    (foreach c (nth 5 e)
      (setq conns (append1 conns (list (or (SfyRtMapName cls "pin" (car c)) (car c)) (cadr c)))))
    (unless (SfyPlaceConn cv obj conns)
      (return "SfyPlaceConn failed"))
    (dbSave cv)
    (return t)))

;; Entry point (spec sections 5.2 and 10.3). Run after (SfyImport path) has created the
;; cellviews: it reads the intents of path, characterizes every class used with Spectre
;; (characterize nil: plan and log only), solves each width, places the class master with
;; the solved w, l, nf and draws the conn stubs. Writes <path>.retarget.log; returns t when
;; every intent was placed.
(procedure (SfyRetarget path map @key (lib nil) (characterize t) (callbacks nil))
  (prog (m classes log entries fixed used plan tab sol cls l vds nf w ok res lsnap params)
    (setq log (outfile (strcat path ".retarget.log") "w"))
    (setq m (SfyRtReadMap map))
    (unless (car m)
      (fprintf log "map %s: %s\nFAIL\n" map (cadr m))
      (close log)
      (return nil))
    (setq classes (car m))
    (setq entries (SfyRtReadIntents path))
    (when lib
      (setq entries (setof e entries (equal (nth 0 e) lib))))
    ;; Fixed-size placeholders (no gmid, section 10.5) need no characterization.
    (setq fixed (setof e entries (null (assoc "gmid" (nth 3 (nth 6 e))))))
    (setq entries (setof e entries (assoc "gmid" (nth 3 (nth 6 e)))))
    (setq ok t)
    ;; Classes used, each with its snapped lengths and drain voltages (vdd/2 always).
    (foreach e entries
      (setq cls (exists c classes (equal (nth 1 (car c)) (nth 2 (nth 6 e)))))
      (if (null cls)
        (progn
          (fprintf log "inst %s: class %s is not in the map\n" (nth 2 (nth 4 e)) (nth 2 (nth 6 e)))
          (setq ok nil))
        (let ((c (car cls)) (row nil))
          (setq row (assoc (nth 1 (car c)) used))
          (unless row
            (setq row (list (nth 1 (car c)) c nil (list (quotient (SfyRtNum (SfyRtAttr c "vdd")) 2.0))))
            (setq used (cons row used)))
          (setq lsnap (SfyRtSnapL (SfyRtField (nth 6 e) "l") (and (SfyRtAttr c "lgrid") (SfyRtNum (SfyRtAttr c "lgrid")))))
          (setq vds (SfyRtField (nth 6 e) "vds"))
          (setq used (cons (list (car row) c (cons lsnap (nth 2 row)) (if vds (cons vds (nth 3 row)) (nth 3 row)))
                           (setof u used (nequal (car u) (car row))))))))
    (setq plan nil)
    (foreach u (reverse used)
      (setq plan (cons (list (car u) (cadr u) (SfyRtUniq (nth 2 u)) (SfyRtUniq (nth 3 u))) plan)))
    (setq plan (reverse plan))
    (foreach p plan
      (fprintf log "class %s L %L VDS %L VGS 0..vdd in 72 steps\n" (car p) (nth 2 p) (nth 3 p)))
    (unless characterize
      (fprintf log "plan only (characterize nil): %d fixed\n" (length fixed))
      (close log)
      (return ok))
    (foreach e fixed
      (setq cls (car (exists c classes (equal (nth 1 (car c)) (nth 2 (nth 6 e))))))
      (setq params (if cls (SfyRtFixedParams cls (nth 6 e)) "its class is not in the map"))
      (setq res (if (stringp params) params (SfyRtPlace e cls params callbacks)))
      (if (equal res t)
        (fprintf log "inst %s: fixed %L\n" (nth 2 (nth 4 e)) params)
        (progn
          (fprintf log "inst %s: not placed: %s\n" (nth 2 (nth 4 e)) res)
          (setq ok nil))))
    ;; Characterize, then solve and place each intent of the class.
    (foreach p plan
      (setq tab (SfyRtCharacterize (nth 1 p) (nth 2 p) (nth 3 p) (strcat path ".rt." (car p))))
      (if (stringp tab)
        (progn
          (fprintf log "class %s: characterization failed: %s\n" (car p) tab)
          (setq ok nil))
        (foreach e entries
          (when (equal (nth 2 (nth 6 e)) (car p))
            (setq cls (nth 1 p))
            (setq l (SfyRtSnapL (SfyRtField (nth 6 e) "l") (and (SfyRtAttr cls "lgrid") (SfyRtNum (SfyRtAttr cls "lgrid")))))
            (setq vds (SfyRtField (nth 6 e) "vds"))
            (setq sol (SfyRtSolve tab (SfyRtField (nth 6 e) "gmid") l (SfyRtField (nth 6 e) "id") vds))
            (if (null sol)
              (progn
                (fprintf log "inst %s: gm/Id %s not reachable at L %L\n" (nth 2 (nth 4 e))
                  (cadr (assoc "gmid" (nth 3 (nth 6 e)))) l)
                (setq ok nil))
              (progn
                (setq w (nth 2 sol))
                (setq nf (SfyRtFingerRule w (or (atoi (cadr (assoc "nf" (nth 3 (nth 6 e))))) 1)
                  (and (SfyRtAttr cls "fingerw_max") (SfyRtNum (SfyRtAttr cls "fingerw_max")))
                  (and (SfyRtAttr cls "fingerw_min") (SfyRtNum (SfyRtAttr cls "fingerw_min")))
                  (and (SfyRtAttr cls "split_above") (SfyRtNum (SfyRtAttr cls "split_above")))))
                (fprintf log "inst %s: intent %L -> W %L nf %d L %L VGS %L residual %L\n" (nth 2 (nth 4 e))
                  (nth 3 (nth 6 e)) w nf l (nth 0 sol) (nth 3 sol))
                (setq res (SfyRtPlace e cls (SfyRtSizeParams cls w nf l) callbacks))
                (unless (equal res t)
                  (fprintf log "inst %s: not placed: %s\n" (nth 2 (nth 4 e)) res)
                  (setq ok nil))))))))
    (fprintf log (if ok "PASS\n" "FAIL\n"))
    (close log)
    (return ok)))

;; ---- BEGIN GENERATED by plugins/interop/tests/retarget.rs (SCHEMIFY_BLESS=1 rewrites it)
(procedure (SfyRtSelfData)
  '(9.0e-1
    (1.5e-7 5.0e-7 2.0e-6)
    (3.0e-1 9.0e-1 1.5e0)
    (0.0e0 1.0e-1 2.0e-1 3.0000000000000004e-1 4.0e-1 5.0e-1 6.000000000000001e-1 7.000000000000001e-1 8.0e-1 9.0e-1 1.0e0 1.1e0 1.2000000000000002e0 1.3e0 1.4000000000000001e0 1.5e0 1.6e0 1.7000000000000002e0 1.8e0)
    1.0e-6
    ((((4.919136602020235e-12 1.462906817444439e-10 4.775860778660421e-13 1.32e-15)
       (9.602936391797045e-11 2.8498043697817786e-9 9.323239215336936e-12 1.32e-15)
       (1.8480087604212405e-9 5.433870691741719e-8 1.7941832625448932e-10 1.32e-15)
       (3.348291238970522e-8 9.472576342617103e-7 3.2507681931752642e-9 1.32e-15)
       (4.869695088442683e-7 1.2006015931860413e-5 4.7278593091676535e-8 1.32e-15)
       (4.133117528376679e-6 7.359945610570974e-5 4.0127354644433775e-7 1.32e-15)
       (1.7543602590380444e-5 2.0201284332079848e-4 1.703262387415577e-6 1.32e-15)
       (4.514312704196861e-5 3.5034150369772177e-4 4.382827868152292e-6 1.32e-15)
       (8.753519497380332e-5 4.969563571923047e-4 8.498562618815855e-6 1.32e-15)
       (1.444513852704505e-4 6.410949386235148e-4 1.402440633693694e-5 1.32e-15)
       (2.1572049049375437e-4 7.841923738835083e-4 2.0943736941141197e-5 1.32e-15)
       (3.012789297171515e-4 9.269475781191915e-4 2.925038152593704e-5 1.32e-15)
       (4.0110679333289995e-4 1.0696015128352527e-3 3.894240711969902e-5 1.32e-15)
       (5.151983390597067e-4 1.212227177370236e-3 5.001925621938899e-5 1.32e-15)
       (6.435519892463687e-4 1.3548452389806726e-3 6.248077565498723e-5 1.32e-15)
       (7.861673242099444e-4 1.497461308431047e-3 7.632692468057712e-5 1.32e-15)
       (9.430442348550686e-4 1.6400768658377213e-3 9.155769270437558e-5 1.32e-15)
       (1.1141826933107318e-3 1.7826922935707827e-3 1.0817307702045938e-4 1.32e-15)
       (1.2995826925523537e-3 1.9253076888486637e-3 1.2617307694683046e-4 1.32e-15))
      ((5.205688248739861e-12 1.5481246903052803e-10 4.775860778660421e-13 1.32e-15)
       (1.0162330744717261e-10 3.0158123913224648e-9 9.323239215336936e-12 1.32e-15)
       (1.955659756173934e-9 5.750406848542208e-8 1.7941832625448932e-10 1.32e-15)
       (3.543337330561038e-8 1.00243769062647e-6 3.2507681931752642e-9 1.32e-15)
       (5.153366646992743e-7 1.2705395500706652e-5 4.7278593091676535e-8 1.32e-15)
       (4.373881656243282e-6 7.788680306332391e-5 4.0127354644433775e-7 1.32e-15)
       (1.8565560022829792e-5 2.1378058176667024e-4 1.703262387415577e-6 1.32e-15)
       (4.777282376285999e-5 3.7074974663156964e-4 4.382827868152292e-6 1.32e-15)
       (9.263433254509284e-5 5.25905271203507e-4 8.498562618815855e-6 1.32e-15)
       (1.5286602907261268e-4 6.784402748540109e-4 1.402440633693694e-5 1.32e-15)
       (2.282867326584391e-4 8.298734830417709e-4 2.0943736941141197e-5 1.32e-15)
       (3.1882915863271376e-4 9.80944524417397e-4 2.925038152593704e-5 1.32e-15)
       (4.2447223760471933e-4 1.131908397078083e-3 3.894240711969902e-5 1.32e-15)
       (5.4520989279134e-4 1.2828423527510267e-3 5.001925621938899e-5 1.32e-15)
       (6.81040454639361e-4 1.4337682626106149e-3 6.248077565498723e-5 1.32e-15)
       (8.319634790182909e-4 1.584692064261982e-3 7.632692468057712e-5 1.32e-15)
       (9.97978850477694e-4 1.7356153240418602e-3 9.155769270437558e-5 1.32e-15)
       (1.1790865395230074e-3 1.8865384465943235e-3 1.0817307702045938e-4 1.32e-15)
       (1.3752865387204522e-3 2.037461534801013e-3 1.2617307694683046e-4 1.32e-15))
      ((5.4922398954594844e-12 1.633342563166121e-10 4.775860778660421e-13 1.32e-15)
       (1.0721725097637475e-10 3.1818204128631505e-9 9.323239215336936e-12 1.32e-15)
       (2.063310751926627e-9 6.066943005342696e-8 1.7941832625448932e-10 1.32e-15)
       (3.738383422151554e-8 1.0576177469912298e-6 3.2507681931752642e-9 1.32e-15)
       (5.437038205542801e-7 1.3404775069552886e-5 4.7278593091676535e-8 1.32e-15)
       (4.614645784109885e-6 8.217415002093806e-5 4.0127354644433775e-7 1.32e-15)
       (1.9587517455279133e-5 2.2554832021254195e-4 1.703262387415577e-6 1.32e-15)
       (5.040252048375136e-5 3.9115798956541745e-4 4.382827868152292e-6 1.32e-15)
       (9.773347011638233e-5 5.54854185214709e-4 8.498562618815855e-6 1.32e-15)
       (1.6128067287477483e-4 7.157856110845067e-4 1.402440633693694e-5 1.32e-15)
       (2.4085297482312376e-4 8.755545922000335e-4 2.0943736941141197e-5 1.32e-15)
       (3.363793875482759e-4 1.0349414707156023e-3 2.925038152593704e-5 1.32e-15)
       (4.4783768187653866e-4 1.1942152813209132e-3 3.894240711969902e-5 1.32e-15)
       (5.752214465229734e-4 1.3534575281318171e-3 5.001925621938899e-5 1.32e-15)
       (7.185289200323533e-4 1.5126912862405567e-3 6.248077565498723e-5 1.32e-15)
       (8.77759633826637e-4 1.6719228200929167e-3 7.632692468057712e-5 1.32e-15)
       (1.0529134661003193e-3 1.8311537822459992e-3 9.155769270437558e-5 1.32e-15)
       (1.2439903857352828e-3 1.990384599617864e-3 1.0817307702045938e-4 1.32e-15)
       (1.4509903848885504e-3 2.1496153807533617e-3 1.2617307694683046e-4 1.32e-15)))
     (((1.4456530577005098e-12 4.299241685829434e-11 4.29827470079438e-14 4.4e-15)
       (2.8221445104824907e-11 8.375104686727616e-10 8.390915293803243e-13 4.4e-15)
       (5.430992735723393e-10 1.5969249110584643e-8 1.6147649362904042e-11 4.4e-15)
       (9.840075320741524e-9 2.783833843602133e-7 2.9256913738577374e-10 4.4e-15)
       (1.4311230128850487e-7 3.5283699248292687e-6 4.255073378250889e-9 4.4e-15)
       (1.2146550250870106e-6 2.162966540116343e-5 3.611461917999041e-8 4.4e-15)
       (5.155775246706951e-6 5.9368240459422994e-5 1.5329361486740194e-7 4.4e-15)
       (1.3266819956896988e-5 1.029595856012625e-4 3.944545081337063e-7 4.4e-15)
       (2.5725149047155594e-5 1.4604727118651517e-4 7.64870635693427e-7 4.4e-15)
       (4.245187798190813e-5 1.8840722128285237e-4 1.262196570324325e-6 4.4e-15)
       (6.339669172083441e-5 2.304611957034349e-4 1.8849363247027076e-6 4.4e-15)
       (8.85409048790114e-5 2.724145940744459e-4 2.6325343373343334e-6 4.4e-15)
       (1.1787866635132892e-4 3.1433823100507867e-4 3.5048166407729113e-6 4.4e-15)
       (1.5140828857609047e-4 3.5625355979608785e-4 4.50173305974501e-6 4.4e-15)
       (1.8912930790764639e-4 3.981666542130578e-4 5.623269808948852e-6 4.4e-15)
       (2.3104160100810694e-4 4.40079163167066e-4 6.8694232212519416e-6 4.4e-15)
       (2.7714513581614486e-4 4.819915216398817e-4 8.240192343393802e-6 4.4e-15)
       (3.274399041409306e-4 5.239038420037631e-4 9.735576931841345e-6 4.4e-15)
       (3.8192590391805583e-4 5.658161528296024e-4 1.1355576925214743e-5 4.4e-15))
      ((1.471442705905276e-12 4.375937771404192e-11 4.29827470079438e-14 4.4e-15)
       (2.87249000224531e-11 8.524511906114234e-10 8.390915293803243e-13 4.4e-15)
       (5.527878631900817e-10 1.625413165170508e-8 1.6147649362904042e-11 4.4e-15)
       (1.0015616803172989e-8 2.8334958943304173e-7 2.9256913738577374e-10 4.4e-15)
       (1.456653453154554e-7 3.5913140860254304e-6 4.255073378250889e-9 4.4e-15)
       (1.236323796595005e-6 2.2015526627348705e-5 3.611461917999041e-8 4.4e-15)
       (5.247751415627393e-6 6.042733691955146e-5 1.5329361486740194e-7 4.4e-15)
       (1.3503492661777211e-5 1.0479632746530882e-4 3.944545081337063e-7 4.4e-15)
       (2.618407142857165e-5 1.486526734475234e-4 7.64870635693427e-7 4.4e-15)
       (4.320919592410272e-5 1.91768301543597e-4 1.262196570324325e-6 4.4e-15)
       (6.452765351565602e-5 2.3457249552767857e-4 1.8849363247027076e-6 4.4e-15)
       (9.012042548141201e-5 2.772743192412844e-4 2.6325343373343334e-6 4.4e-15)
       (1.1998155633579265e-4 3.199458505869334e-4 3.5048166407729113e-6 4.4e-15)
       (1.541093284119375e-4 3.62608925580359e-4 4.50173305974501e-6 4.4e-15)
       (1.9250326979301568e-4 4.0526972633975265e-4 5.623269808948852e-6 4.4e-15)
       (2.351632549408581e-4 4.479299311918501e-4 6.8694232212519416e-6 4.4e-15)
       (2.820892512221812e-4 4.905899828782542e-4 8.240192343393802e-6 4.4e-15)
       (3.3328125030003536e-4 5.332499957758817e-4 9.735576931841345e-6 4.4e-15)
       (3.887392500731847e-4 5.759099989653138e-4 1.1355576925214743e-5 4.4e-15))
      ((1.4972323541100426e-12 4.4526338569789483e-11 4.29827470079438e-14 4.4e-15)
       (2.92283549400813e-11 8.673919125500851e-10 8.390915293803243e-13 4.4e-15)
       (5.624764528078241e-10 1.6539014192825523e-8 1.6147649362904042e-11 4.4e-15)
       (1.0191158285604453e-8 2.8831579450587007e-7 2.9256913738577374e-10 4.4e-15)
       (1.4821838934240594e-7 3.654258247221592e-6 4.255073378250889e-9 4.4e-15)
       (1.2579925681029992e-6 2.2401387853533983e-5 3.611461917999041e-8 4.4e-15)
       (5.339727584547834e-6 6.148643337967991e-5 1.5329361486740194e-7 4.4e-15)
       (1.3740165366657436e-5 1.0663306932935512e-4 3.944545081337063e-7 4.4e-15)
       (2.664299380998771e-5 1.5125807570853157e-4 7.64870635693427e-7 4.4e-15)
       (4.396651386629732e-5 1.9512938180434164e-4 1.262196570324325e-6 4.4e-15)
       (6.565861531047766e-5 2.386837953519222e-4 1.8849363247027076e-6 4.4e-15)
       (9.16999460838126e-5 2.821340444081229e-4 2.6325343373343334e-6 4.4e-15)
       (1.220844463202564e-4 3.255534701687881e-4 3.5048166407729113e-6 4.4e-15)
       (1.568103682477845e-4 3.689642913646302e-4 4.50173305974501e-6 4.4e-15)
       (1.95877231678385e-4 4.123727984664474e-4 5.623269808948852e-6 4.4e-15)
       (2.3928490887360928e-4 4.557806992166343e-4 6.8694232212519416e-6 4.4e-15)
       (2.870333666282175e-4 4.991884441166267e-4 8.240192343393802e-6 4.4e-15)
       (3.391225964591402e-4 5.425961495480004e-4 9.735576931841345e-6 4.4e-15)
       (3.955525962283135e-4 5.860038451010253e-4 1.1355576925214743e-5 4.4e-15)))
     (((3.5899548490593066e-13 1.0676201634347253e-11 2.6864216879964874e-15 1.76e-14)
       (7.0081623776785846e-12 2.0797692448644088e-10 5.244322058627027e-14 1.76e-14)
       (1.3486651311642148e-10 3.96560453941612e-9 1.0092280851815026e-12 1.76e-14)
       (2.443561816207432e-9 6.913026436447569e-8 1.828557108661086e-11 1.76e-14)
       (3.5538727444599614e-8 8.761914660951772e-7 2.6594208614068056e-10 1.76e-14)
       (3.0163230894287826e-7 5.371241860335989e-6 2.2571636987494006e-9 1.76e-14)
       (1.2803210458404467e-6 1.474276982171871e-5 9.580850929212622e-9 1.76e-14)
       (3.294516923141727e-6 2.5567701850561292e-5 2.4653406758356644e-8 1.76e-14)
       (6.388263288531145e-6 3.626756133465928e-5 4.7804414730839186e-8 1.76e-14)
       (1.0541970938396289e-5 4.678670404626829e-5 7.88872856452703e-8 1.76e-14)
       (1.5743145261944073e-5 5.72298645673359e-5 1.1780852029391923e-7 1.76e-14)
       (2.19871461632778e-5 6.76480492842204e-5 1.6453339608339584e-7 1.76e-14)
       (2.9272520651788756e-5 7.80588434154708e-5 2.1905104004830696e-7 1.76e-14)
       (3.759884965941197e-5 8.846757440674657e-5 2.813583162340631e-7 1.76e-14)
       (4.696601805015823e-5 9.887575054138684e-5 3.5145436305930325e-7 1.76e-14)
       (5.737399519583132e-5 1.09283781289443e-4 4.2933895132824635e-7 1.76e-14)
       (6.882277313472033e-5 1.1969177466887301e-4 5.150120214621126e-7 1.76e-14)
       (8.131234983281657e-5 1.3009975858480465e-4 6.084735582400841e-7 1.76e-14)
       (9.484272477747065e-5 1.4050774013217766e-4 7.097235578259215e-7 1.76e-14))
      ((3.6060733791872857e-13 1.0724136687831477e-11 2.6864216879964874e-15 1.76e-14)
       (7.039628310030346e-12 2.0891071960760724e-10 5.244322058627027e-14 1.76e-14)
       (1.3547204996753037e-10 3.983409698236148e-9 1.0092280851815026e-12 1.76e-14)
       (2.4545331588593984e-9 6.944065218152745e-8 1.828557108661086e-11 1.76e-14)
       (3.569829269628402e-8 8.801254761699373e-7 2.6594208614068056e-10 1.76e-14)
       (3.029866071621279e-7 5.395358186972569e-6 2.2571636987494006e-9 1.76e-14)
       (1.2860695563979741e-6 1.4808963350476737e-5 9.580850929212622e-9 1.76e-14)
       (3.3093089671967406e-6 2.568249821706418e-5 2.4653406758356644e-8 1.76e-14)
       (6.416945937369647e-6 3.643039897597229e-5 4.7804414730839186e-8 1.76e-14)
       (1.0589303309783451e-5 4.699677156256483e-5 7.88872856452703e-8 1.76e-14)
       (1.5813830374120426e-5 5.748682080635112e-5 1.1780852029391923e-7 1.76e-14)
       (2.2085866200927835e-5 6.795178210714779e-5 1.6453339608339584e-7 1.76e-14)
       (2.9403951275817737e-5 7.840931963933671e-5 2.1905104004830696e-7 1.76e-14)
       (3.7767664649152406e-5 8.88647847682635e-5 2.813583162340631e-7 1.76e-14)
       (4.717689066799381e-5 9.931969254930526e-5 3.5145436305930325e-7 1.76e-14)
       (5.763159856662827e-5 1.09774454290992e-4 4.2933895132824635e-7 1.76e-14)
       (6.91317803475976e-5 1.202291784962713e-4 5.150120214621126e-7 1.76e-14)
       (8.167743396776064e-5 1.3068389319556206e-4 6.084735582400841e-7 1.76e-14)
       (9.526855891216619e-5 1.4113860551565963e-4 7.097235578259215e-7 1.76e-14))
      ((3.6221919093152644e-13 1.0772071741315698e-11 2.6864216879964874e-15 1.76e-14)
       (7.071094242382108e-12 2.0984451472877358e-10 5.244322058627027e-14 1.76e-14)
       (1.3607758681863927e-10 4.001214857056175e-9 1.0092280851815026e-12 1.76e-14)
       (2.4655045015113644e-9 6.975103999857922e-8 1.828557108661086e-11 1.76e-14)
       (3.5857857947968424e-8 8.840594862446973e-7 2.6594208614068056e-10 1.76e-14)
       (3.043409053813775e-7 5.419474513609148e-6 2.2571636987494006e-9 1.76e-14)
       (1.2918180669555016e-6 1.4875156879234766e-5 9.580850929212622e-9 1.76e-14)
       (3.3241010112517543e-6 2.5797294583567074e-5 2.4653406758356644e-8 1.76e-14)
       (6.4456285862081515e-6 3.65932366172853e-5 4.7804414730839186e-8 1.76e-14)
       (1.0636635681170613e-5 4.720683907886136e-5 7.88872856452703e-8 1.76e-14)
       (1.5884515486296778e-5 5.774377704536635e-5 1.1780852029391923e-7 1.76e-14)
       (2.218458623857787e-5 6.825551493007519e-5 1.6453339608339584e-7 1.76e-14)
       (2.9535381899846722e-5 7.875979586320263e-5 2.1905104004830696e-7 1.76e-14)
       (3.7936479638892836e-5 8.926199512978044e-5 2.813583162340631e-7 1.76e-14)
       (4.738776328582939e-5 9.976363455722367e-5 3.5145436305930325e-7 1.76e-14)
       (5.788920193742522e-5 1.1026512729254103e-4 4.2933895132824635e-7 1.76e-14)
       (6.944078756047486e-5 1.2076658232366958e-4 5.150120214621126e-7 1.76e-14)
       (8.204251810270467e-5 1.3126802780631948e-4 6.084735582400841e-7 1.76e-14)
       (9.569439304686175e-5 1.417694708991416e-4 7.097235578259215e-7 1.76e-14))))
    ((2.6074235381862863e1 5.0e-7 nil 9.144832350616542e-6 (3.609572563882043e-1 5.121705942775477e-2 1.785505152539256e-4 -5.9207020674396205e-2))
     (7.290596803176875e0 2.3630638751879537e-7 9.0e-1 1.6201698481222408e-5 (7.225627122968747e-1 3.442900875453814e1 4.7058277502941e-7 -5.844024785815982e-2))
     (2.5807209077046295e1 1.413035677120753e-6 9.729247445108848e-1 1.1747899873877373e-6 (3.683007616815135e-1 2.1754035453376074e-2 5.400331308209844e-5 -5.368147546295887e-2))
     (7.105386006994673e0 5.0e-7 3.6620668950588775e-1 2.794717582627904e-5 (7.314522668790342e-1 1.6370984827637407e1 1.707116347643226e-6 -7.144100856148672e-2))
     (2.359742434872369e1 2.5706062305521616e-7 nil 1.8830739926267793e-5 (4.154386223392335e-1 4.073507788111298e-1 4.6227332573724524e-5 -1.5069193243556e-1))
     (2.9599339449720347e1 6.636963357903392e-7 3.0e-1 2.9817781192725707e-5 (1.282775044059342e-1 4.89950568788404e-5 6.085875411158708e-1 -2.5501074948408586e-4))
     (3.6518683054174854e0 5.0e-7 3.6618897996684735e-1 1.2643072972749736e-6 (9.979270476487667e-1 6.299556479710123e1 2.006978271164181e-8 -1.5744171846567845e-3))
     (2.612005109584495e1 4.520755429367831e-7 5.555026887205614e-1 5.192900663047856e-6 (3.596972758841287e-1 5.4387084908402154e-2 9.548040075679097e-5 -5.988894824084312e-2))
     (1.3860962589251336e1 1.1903994857721557e-6 nil 1.747520983721591e-5 (5.627156438472699e-1 1.2698496057035173e0 1.376163740865546e-5 -3.1365001728467945e-1))
     (2.132971125793003e1 2.0e-6 3.0e-1 9.103641114336938e-6 (4.4855695756076114e-1 1.0038925562512914e-1 9.068342082674175e-5 -2.780670542659003e-1))
     (1.755591030541746e1 3.715721031120282e-7 1.1990478512970044e0 2.251355081890545e-6 (5.039943660714409e-1 1.812412567095265e0 1.2421868633910262e-6 -5.607138820896296e-2))
     (1.1533605930384223e1 2.506008719335374e-7 4.109055901537631e-1 1.2585501421954589e-6 (5.997027319867916e-1 1.044300767185346e1 1.2051606029052041e-7 -3.772376595422955e-3))
     (2.258743257375263e1 5.0e-7 nil 6.389913113661439e-5 (4.30188833161878e-1 2.778068022937038e-1 2.3001283845115766e-4 -2.3935163798613246e-1))
     (1.287225980479533e1 1.498347939026273e-6 9.0e-1 4.1401388523906985e-6 (5.784284212134453e-1 1.2619676479897055e0 3.2807012596447096e-6 -2.2189338717259588e-1))
     (2.865476879527389e1 2.3945811740467417e-7 1.1626802126406102e0 3.5309960135602e-6 (2.673006659767139e-1 8.559459849256188e-3 4.125255653681279e-4 -4.716287231577354e-3))
     (1.7015013211346947e1 1.5e-7 1.1991238997499585e0 7.10997027895961e-5 (5.125904737700933e-1 5.391042298583669e0 1.3188489136558132e-5 -1.5886004919737218e-1))
     (8.566888747718764e0 3.8021157900022976e-7 nil 1.3406396144070079e-5 (6.785253359222042e-1 1.4693486436996988e1 9.124040234803551e-7 -1.1995693135737895e-1))
     (9.61305616170496e0 1.3471942043004427e-6 9.0e-1 4.550435984147813e-6 (6.506588800032949e-1 3.0993476428975564e0 1.46819153849216e-6 -1.8437354276396434e-1))
     (2.1653197573267725e1 5.0e-7 7.761667028458079e-1 4.783980981034724e-6 (4.43832670227225e-1 3.705866093372316e-1 1.2909211667390092e-5 -2.754741842216646e-1))
     (2.8856439736931105e1 8.227815049385222e-7 6.792981467761205e-1 4.052570220944692e-6 (2.4918344485546817e-1 1.3799597653530351e-3 2.9367307096145106e-3 -5.368806666194104e-3))
     (9.940794345299835e0 1.8756447632213547e-7 nil 3.885989671999462e-5 (6.419290137084817e-1 2.1825108784578482e1 1.780513311688638e-6 -1.8167705081348728e-1))
     (5.658651040560252e0 1.5e-7 3.0e-1 3.092921580599811e-5 (8.014984203164335e-1 8.819467254785685e1 3.506925635356837e-7 -2.3409189480219084e-3))
     (2.9233093406494778e1 2.299955327723115e-7 6.733809709114063e-1 6.903186815330843e-6 (2.1534655312709738e-1 1.9171436711359758e-3 3.60076655665585e-3 -2.8028614201573987e-3))
     (2.1342380004637967e1 1.7102448922621247e-6 1.1475905862242815e0 9.855171388877526e-6 (4.483719395344449e-1 1.1801190748753576e-1 8.350997453302256e-5 -2.780595873861422e-1))
     (1.8939037956847415e1 2.0e-6 nil 8.793935282821081e-5 (4.8347103911359995e-1 2.1276804959278175e-1 4.13310894170993e-4 -1.4793289622878092e-1))
     (8.530900683013908e0 1.4721250665225943e-6 9.0e-1 8.741702056222395e-6 (6.79483939493007e-1 3.7198168853238e0 2.3500355866204026e-6 -1.1585800119805079e-1))
     (1.5202107120906597e1 1.8090274151719214e-6 1.1031938307326286e0 1.3453547102346778e-6 (5.414017507612325e-1 6.114584953295225e-1 2.2002388069032383e-6 -3.356267327023357e-1))
     (2.036370786509946e1 1.5e-7 8.846157523560076e-1 1.4253867927554719e-5 (4.626647493792134e-1 1.9655893503648023e0 7.251701849579762e-6 -2.5650453263023465e-1))
     (4.532783696673543e0 2.0203474436221034e-7 nil 1.6780994694711325e-5 (8.923614877610763e-1 1.0763757509413864e2 1.5590275682107155e-7 -1.038472867158724e-2))
     (1.8177220410893618e1 2.874504624872181e-7 1.5e0 2.181085871977269e-6 (4.9459684200593557e-1 2.0371360035928374e0 1.0706628659699457e-6 -5.416041202682109e-2))
     (2.1508858218118895e1 1.5e-7 1.0973498786428955e0 2.9491409740291246e-5 (4.459406437486042e-1 1.401423804196986e0 2.1043890971432288e-5 -2.7725054503670776e-1))
     (2.152121288314248e0 2.0949110230853562e-7 1.1735203561739294e0 3.6393788599850734e-6 (1.3810798695817665e0 4.692261861194184e2 7.7561290644995e-9 -2.088220428354326e-3))
     (3.689118958848259e0 2.018522137996269e-7 nil 3.0480690935578022e-6 (9.932876034997852e-1 1.627313898676031e2 1.8730676951986252e-8 -4.871346892174611e-3))
     (1.914574833579798e1 5.0e-7 9.0e-1 1.4602940950456986e-5 (4.8045218119385413e-1 8.139107446196366e-1 1.7941698210755715e-5 -1.6916701573028803e-1))
     (8.917225643897124e0 9.74159069156447e-7 7.901906711128035e-1 9.943454547496391e-6 (6.691935143192704e-1 5.120093651608458e0 1.9420454437142364e-6 -1.5347481092354975e-1))
     (8.008774030778365e0 4.861460758291149e-7 9.151316448768108e-1 1.572812693039349e-5 (6.933916746643506e-1 1.3071604434348407e1 1.2032284949707097e-6 -4.307316637270908e-2))
     (2.3954144002855823e1 5.0e-7 nil 1.0097871477657267e-5 (4.102289857526577e-1 1.8128447199201153e-1 5.57018004173145e-5 -1.0660547807628973e-1))
     (1.8770774311880903e1 1.634548773133862e-7 1.5e0 6.734846510617321e-5 (4.8592840985517244e-1 3.112944755950063e0 2.1634969582240023e-5 -1.2930893630715445e-1))
     (1.1101223114939954e1 1.3130977914661721e-6 9.624864374024258e-1 1.0026163298884897e-6 (6.110190114240508e-1 2.1889387664778246e0 4.5803763231886955e-7 -7.623842653051227e-2))
     (2.878134036122033e1 5.0e-7 7.164287536896994e-1 9.346064402734124e-6 (2.559300390084931e-1 2.7790089914835454e-3 3.3630925381586563e-3 -5.290107980783887e-3))
     (1.8720943991351824e1 9.75380259032452e-7 nil 1.078588604212269e-6 (4.866561462104362e-1 4.718727291749086e-1 2.285761684296169e-6 -1.2356442688887981e-1))
     (5.63598770214781e0 8.702074252144391e-7 9.0e-1 7.859329443122078e-6 (8.03327463904803e-1 1.5176298712301596e1 5.178686577084482e-7 -5.094030414647399e-3))
     (2.9225486307761948e1 1.5e-7 6.047444185351212e-1 2.272893337826654e-6 (2.1602994106936266e-1 3.027211563112182e-3 7.50820776956191e-4 -2.903783631609258e-3))
     (2.5549165556442183e1 2.0087683697071034e-7 1.176186563476535e0 2.9247147793790837e-6 (3.7539723092547866e-1 2.0045033059603992e-1 1.4590720657244274e-5 -4.5844096529986444e-2))
     (2.039589387984334e0 1.1313788133391844e-6 nil 6.700082127605514e-6 (1.4327541890232796e0 8.977992359812391e1 7.462784394422811e-8 -2.22222213546841e-3))
     (1.4175582282849355e1 5.0e-7 1.5e0 1.0764790426200313e-6 (5.577156081659301e-1 2.897612735854291e0 3.7150549115827845e-7 -3.2971860827202804e-1))
     (2.010204822941356e1 1.7301293329815918e-6 5.89082956108098e-1 4.680241748912354e-6 (4.664861020731539e-1 1.708811019871778e-1 2.7388878550558156e-5 -2.432900678040113e-1))
     (1.453936422769054e1 7.840153275080792e-7 7.493982127131816e-1 6.596793096301832e-6 (5.519342704383653e-1 1.6541859179242764e0 3.987939339115939e-6 -3.4008687102349633e-1))
     (4.0e1 5.0e-7 nil 1.0e-6 nil)
     (1.5e1 3.0e-6 nil 1.0e-6 nil)
     (1.5e1 5.0e-7 2.0e0 1.0e-6 nil))
    ((5.0e-7 3.0e-8 5.1e-7) (7.3e-7 5.0e-9 7.3e-7) (1.0e-9 3.0e-8 3.0e-8))
    ((1.0e-5 1 2.0e-6 5) (1.01e-5 1 2.0e-6 6) (1.0e-6 4 2.0e-6 4) (3.0e-6 2 nil 2))))
;; ---- END GENERATED
