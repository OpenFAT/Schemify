;; sfy_cells.il: cells, CDF, schematics, symbols and text views to and from .sfyx
;; (interop spec sections 4.2, 5.2, 5.3, 6.2). Entry points: SfyExport, SfyExportLib,
;; SfyImport.
;;
;; Every decision about meaning is made in Rust; this file only copies database objects to
;; records and back. Prefix form only (spec 5.1). Dynamic variables, let-bound by the entry
;; points and read by the procedures they call: the log state of sfy_io.il; during an
;; import also sfyOverwrite sfyCallbacks sfyCheck (the keyword arguments), sfyTargets (dump
;; library/cell -> target library of every design cell), sfyMasters ((key cellview) of every
;; master opened, closed when the import ends), and
;; per cellview sfyCv sfyUpi sfyRefs (ref -> created object, 0 = the cellview) sfyTerms.

(procedure (SfyProducer) "schemify.il 1")

;; ================================================================ export

;; Dumps cells (names; nil = every cell) of library lib to path, with the closure of their
;; schematic masters inside lib as "design" cells and, with ref, every master outside lib
;; as a "ref" cell (the views instances use, plus CDF). Writes <path>.log; t on PASS.
(procedure (SfyExport path lib cells @key (ref t) (maestro t))
  (let ((sfyLogPort (SfyLogOpen path)) (sfyCountTab (makeTable "sfyCounts" nil))
        (sfyCountKeys nil) (sfyFails 0) (design nil) (refs nil))
    (SfyLog (sprintf nil "SfyExport %s library %s" path lib))
    (SfyPreflight)
    (if (null (ddGetObj lib))
      (SfyRefuse 0 (sprintf nil "library %s is not in cds.lib" lib))
      ;; Backstop: an unexpected SKILL error still ends the log with its message and FAIL.
      (unless (errset
                (progn
                  (setq design (SfyDesignClosure lib (or cells (SfyCellNames lib))))
                  (when ref (setq refs (SfyRefMasters lib design)))
                  (SfyDumpFile path refs
                    (mapcar (lambda (c) (list lib c nil)) design)
                    (and maestro lib) design))
                t)
        (SfyRefuse 0 (sprintf nil "SKILL error %s" (SfyErrMsg)))))
    (SfyLogClose)))

;; Dumps every cell of PDK library lib that has a symbol as a "ref" cell (symbol, CDF,
;; simInfo). Writes <path>.log; t on PASS.
(procedure (SfyExportLib path lib)
  (let ((sfyLogPort (SfyLogOpen path)) (sfyCountTab (makeTable "sfyCounts" nil))
        (sfyCountKeys nil) (sfyFails 0))
    (SfyLog (sprintf nil "SfyExportLib %s library %s" path lib))
    (SfyPreflight)
    (if (null (ddGetObj lib))
      (SfyRefuse 0 (sprintf nil "library %s is not in cds.lib" lib))
      (unless (errset
                (SfyDumpFile path
                  (mapcar (lambda (c) (list lib c (list "symbol")))
                    (setof c (SfyCellNames lib) (ddGetObj lib c "symbol")))
                  nil nil nil)
                t)
        (SfyRefuse 0 (sprintf nil "SKILL error %s" (SfyErrMsg)))))
    (SfyLogClose)))

;; Writes the file: header, ref cells, design cells, maestro records (sfy_ade.il, when
;; loaded and adeLib is a library), end. refs and design are (lib cell views) entries
;; sorted by (lib cell); views nil means schematic and symbol.
(procedure (SfyDumpFile path refs design adeLib adeCells)
  (let ((port (outfile path "w")))
    (if (null port)
      (SfyRefuse 0 (sprintf nil "cannot write %s" path))
      (unwindProtect
        (progn
          (SfyPut port "sfyx" (list 1 (SfyProducer)))
          (SfyDumpCells port refs "ref")
          (SfyDumpCells port design "design")
          (when (and adeLib (fboundp 'SfyAdeExport))
            (SfyAdeExport port adeLib adeCells))
          (SfyPut port "end" nil))
        (close port)))))

;; Every cell name of lib, sorted.
(procedure (SfyCellNames lib)
  (sort (getSGq (getSGq (ddGetObj lib) cells) name) nil))

;; cells plus every cell of lib their schematics instantiate, transitively; sorted.
(procedure (SfyDesignClosure lib cells)
  (let ((seen nil) (todo nil) (c nil) (cv nil) (m nil))
    (foreach c0 cells
      (cond
        ((null (ddGetObj lib c0)) (SfyRefuse 0 (sprintf nil "no cell %s in library %s" c0 lib)))
        ((not (member c0 seen))
          (setq seen (cons c0 seen))
          (setq todo (cons c0 todo)))))
    (while todo
      (setq c (car todo))
      (setq todo (cdr todo))
      (when (ddGetObj lib c "schematic")
        (setq cv (dbOpenCellViewByType lib c "schematic" nil "r"))
        (when cv
          (foreach i (getSGq cv instances)
            (setq m (getSGq i cellName))
            (when (and (equal (getSGq i libName) lib) (not (member m seen)))
              (setq seen (cons m seen))
              (setq todo (cons m todo))))
          (dbClose cv))))
    (sort seen nil)))

;; (lib cell views) of every master outside lib that the design schematics instantiate
;; (schematic pins excluded), sorted by (lib cell); views sorted.
(procedure (SfyRefMasters lib design)
  (let ((tab (makeTable "sfyRefMasters" nil)) (keys nil) (cv nil) (k nil) (e nil) (v nil))
    (foreach c design
      (when (ddGetObj lib c "schematic")
        (setq cv (dbOpenCellViewByType lib c "schematic" nil "r"))
        (when cv
          (foreach i (getSGq cv instances)
            (unless (or (equal (getSGq i libName) lib) (equal (getSGq i purpose) "pin"))
              (setq k (strcat (getSGq i libName) "/" (getSGq i cellName)))
              (setq e (arrayref tab k))
              (unless e
                (setq e (list (getSGq i libName) (getSGq i cellName) nil))
                (setq keys (cons k keys)))
              (setq v (getSGq i viewName))
              (unless (member v (caddr e))
                (setq e (list (car e) (cadr e) (cons v (caddr e)))))
              (setarray tab k e)))
          (dbClose cv))))
    (mapcar (lambda (x) (list (car x) (cadr x) (sort (caddr x) nil)))
      (mapcar 'cadr
        (SfySortByKey
          (mapcar (lambda (k0) (list (list (car (arrayref tab k0)) (cadr (arrayref tab k0)))
                                     (arrayref tab k0)))
            keys))))))

;; Writes cells, a list of (lib cell views), with a lib record at each library change.
(procedure (SfyDumpCells port cells role)
  (let ((cur nil))
    (foreach e cells
      (unless (equal (car e) cur)
        (setq cur (car e))
        (SfyPut port "lib" (list cur)))
      ;; One cell's SKILL error costs that cell (its records so far stay; the import
      ;; reports what does not fit), logged with its message, not the whole dump.
      (printf "schemify: dumping %s/%s\n" (car e) (cadr e))
      (unless (errset (SfyDumpCell port (car e) (cadr e) role (caddr e)) t)
        (SfyRefuse 0 (sprintf nil "%s/%s: SKILL error %s" (car e) (cadr e) (SfyErrMsg)))))))

;; One cell: cell record, CDF, cellviews by view name, text views (design cells). A master
;; missing from cds.lib (an unbound instance) is refused and skipped, so the rest dumps.
(procedure (SfyDumpCell port lib cell role views)
  (if (null (ddGetObj lib cell))
    (SfyRefuse 0 (sprintf nil "master %s/%s not found (is its library in cds.lib?)" lib cell))
    (progn
      (SfyPut port "cell" (list cell role))
      (SfyDumpCdf port lib cell role)
      (foreach v (or views (setof v0 (list "schematic" "symbol") (ddGetObj lib cell v0)))
        (SfyDumpCv port lib cell v))
      (when (equal role "design") (SfyDumpTextViews port lib cell)))))

;; ---------------------------------------------------------------- CDF

;; The cell's CDF: cdf, its parameters in CDF order, its simInfo by simulator name. A design
;; cell gives its base CDF (what the loader recreates); a ref cell its effective CDF, which
;; carries the library and user layers instances of it actually see.
(procedure (SfyDumpCdf port lib cell role)
  (let ((cdf (if (equal role "ref")
               (cdfGetCellCDF (ddGetObj lib cell))
               (cdfGetBaseCellCDF (ddGetObj lib cell)))))
    (when cdf
      (SfyPut port "cdf" (list (SfyText (list (SfyStrOr (getSGq cdf paramLabelSet))))))
      (foreach p (getSGq cdf parameters)
        (SfyPut port "param" (SfyParamFields p)))
      (foreach s (SfySimInfoFields (getSGq cdf simInfo))
        (SfyPut port "siminfo" s)))))

;; A default value as text: booleans "t"/"nil", the rest through SfyAtomStr.
(procedure (SfyDefStr ty v)
  (if (equal ty "boolean")
    (if (and v (not (member v (list "nil" "FALSE" "false" "no")))) "t" "nil")
    (SfyAtomStr v)))

;; The fields of a param record (spec 4.2). Flags in the order parseAsNumber parseAsCEL
;; storeDefault dontSave hidden; hidden = display "nil" (an unset display means shown).
;; GUESS(G16): flags read back as "yes" or t.
(procedure (SfyParamFields p)
  (let ((ty (getSGq p paramType)) (flags nil) (prompt (getSGq p prompt)))
    (when (equal (getSGq p display) "nil") (setq flags (cons "hidden" flags)))
    (when (SfyYes (getSGq p dontSave)) (setq flags (cons "dontSave" flags)))
    (when (SfyYes (getSGq p storeDefault)) (setq flags (cons "storeDefault" flags)))
    (when (SfyYes (getSGq p parseAsCEL)) (setq flags (cons "parseAsCEL" flags)))
    (when (SfyYes (getSGq p parseAsNumber)) (setq flags (cons "parseAsNumber" flags)))
    (list (getSGq p name) ty (SfyDefStr ty (getSGq p defValue))
      (if (stringp prompt) prompt (getSGq p name))
      (SfyStrOr (getSGq p units)) flags (mapcar 'SfyAtomStr (getSGq p choices))
      (SfyText (list (SfyStrOr (getSGq p callback)))))))

;; ("simulator" fields) of every simulator in simInfo (a disembodied property list
;; (nil sim1 dpl1 sim2 dpl2 ...)), sorted by simulator; fields (key value) sorted by key.
(procedure (SfySimInfoFields dpl)
  (let ((out nil) (rest (cdr dpl)))
    (while rest
      (when (cadr rest)
        (setq out (cons (list (SfyAtomStr (car rest)) (SfyDplFields (cadr rest))) out)))
      (setq rest (cddr rest)))
    (sort out (lambda (a b) (alphalessp (car a) (car b))))))

(procedure (SfyDplFields dpl)
  (let ((out nil) (rest (cdr dpl)))
    (while rest
      (setq out (cons (list (SfyAtomStr (car rest)) (SfySimValue (cadr rest))) out))
      (setq rest (cddr rest)))
    (sort out (lambda (a b) (alphalessp (car a) (car b))))))

;; A simInfo value: an atom as a string; a property list (nil k v ...) as ((k v) ...); any
;; other list as a list of strings; nil as the empty list.
(procedure (SfySimValue v)
  (let ((out nil))
    (cond
      ((null v) nil)
      ((not (listp v)) (SfyAtomStr v))
      ((null (car v))
        (setq v (cdr v))
        (while v
          (setq out (cons (list (SfyAtomStr (car v)) (SfyAtomStr (cadr v))) out))
          (setq v (cddr v)))
        (reverse out))
      (t (mapcar 'SfyAtomStr v)))))

;; ---------------------------------------------------------------- cellviews

(procedure (SfyDumpCv port lib cell view)
  (let ((cv (dbOpenCellViewByType lib cell view nil "r")))
    (if (null cv)
      (SfyRefuse 0 (sprintf nil "cannot open %s/%s/%s" lib cell view))
      (unwindProtect (SfyDumpCvBody port cv view) (dbClose cv)))))

;; A coordinate in database units (spec 3.1, 5.1).
(procedure (SfyDbu v upi)
  (round (times v upi)))

;; Points ((x y) ...) as a flat list of database units.
(procedure (SfyFlatPts pts upi)
  (let ((out nil))
    (foreach p pts
      (setq out (cons (SfyDbu (yCoord p) upi) (cons (SfyDbu (xCoord p) upi) out))))
    (reverse out)))

;; A wire's points: a two-point wire is written from its smaller end (x, then y), so a
;; wire's drawing direction, which nothing reads, never shows as a difference (spec 4.2).
(procedure (SfyWirePts pts upi)
  (let ((f (SfyFlatPts pts upi)))
    (if (and (equal (length f) 4)
             (or (greaterp (car f) (caddr f))
                 (and (equal (car f) (caddr f)) (greaterp (cadr f) (cadddr f)))))
      (list (caddr f) (cadddr f) (car f) (cadr f))
      f)))

;; A box ((x0 y0) (x1 y1)) as (x0 y0 x1 y1) in database units.
(procedure (SfyBoxDbu b upi)
  (SfyFlatPts (list (lowerLeft b) (upperRight b)) upi))

(procedure (SfyLabelKind lt)
  (case lt ("NLPLabel" "NLP") ("ILLabel" "IL") (t "normal")))

;; The coordinates and width of a shape (spec 4.2 shape record).
(procedure (SfyShapeCoords f upi)
  (case (getSGq f objType)
    (("rect" "ellipse") (SfyBoxDbu (getSGq f bBox) upi))
    ("arc"
      (append (SfyBoxDbu (getSGq f ellipseBBox) upi)
        (list (round (times (getSGq f startAngle) 1000000.0))
              (round (times (getSGq f stopAngle) 1000000.0)))))
    (t (SfyFlatPts (getSGq f points) upi))))

(procedure (SfyWidth f upi)
  (if (equal (getSGq f objType) "path") (SfyDbu (getSGq f width) upi) 0))

;; Writes entries (key object fields) sorted by key as (tag ref fields...), numbering refs
;; from n+1 and recording object -> ref in refs; returns the last ref.
(procedure (SfyEmit port tag entries refs n)
  (foreach e (SfySortByKey entries)
    (setq n (add1 n))
    (setarray refs (cadr e) n)
    (SfyPut port tag (cons n (caddr e))))
  n)

;; One cellview in spec 4.2 order: cv, term, pinfig, inst, spin, wire, label, shape, prop.
(procedure (SfyDumpCvBody port cv view)
  (let ((upi 160000) (refs (makeTable "sfyRefs" nil)) (n 0)
        (pinfigs nil) (insts nil) (spins nil) (wires nil) (labels nil) (shapes nil)
        (ot nil) (par nil) (b nil))
    (SfyPut port "cv" (list view (getSGq cv cellViewType) upi))
    (foreach i (getSGq cv instances)
      (if (equal (getSGq i purpose) "pin") (setq spins (cons i spins)) (setq insts (cons i insts))))
    (foreach f (getSGq cv shapes)
      (setq ot (getSGq f objType))
      (cond
        ((equal ot "label") (setq labels (cons f labels)))
        ((getSGq f pin) (setq pinfigs (cons f pinfigs)))
        ((and (equal (getSGq f layerName) "wire") (equal (getSGq f purpose) "drawing"))
          ;; GUESS(G15): solder dots (dot or ellipse on wire drawing) are not dumped.
          (if (member ot (list "line" "path"))
            (setq wires (cons f wires))
            (SfyCount (strcat "skipped:" ot))))
        ((member ot (list "line" "rect" "polygon" "ellipse" "arc" "path"))
          (setq shapes (cons f shapes)))
        (t (SfyCount (strcat "skipped:" ot)))))
    (unless (equal (getSGq cv cellViewType) "schematic")
      (foreach e (SfySortByKey (mapcar (lambda (t0) (list (getSGq t0 name) t0)) (getSGq cv terminals)))
        (SfyPut port "term"
          (list (car e) (getSGq (cadr e) direction) (SfyStrOr (dbGetTermNetExpr (cadr e)))))))
    (setq n (SfyEmit port "pinfig"
      (mapcar (lambda (f)
                (setq b (cons (getSGq (getSGq (getSGq f pin) term) name) (SfyBoxDbu (getSGq f bBox) upi)))
                (list b f b))
        pinfigs)
      refs n))
    (setq n (SfyEmit port "inst"
      (mapcar (lambda (i)
                (list (list (getSGq i name)) i
                  (list (getSGq i name) (getSGq i libName) (getSGq i cellName) (getSGq i viewName)
                    (SfyDbu (xCoord (getSGq i xy)) upi) (SfyDbu (yCoord (getSGq i xy)) upi)
                    (getSGq i orient))))
        insts)
      refs n))
    (setq n (SfyEmit port "spin"
      (mapcar (lambda (i)
                (setq b (getSGq (getSGq i pin) term))
                (list (list (getSGq b name)) i
                  (list (getSGq b name) (getSGq b direction) (getSGq i libName) (getSGq i cellName)
                    (getSGq i viewName) (SfyDbu (xCoord (getSGq i xy)) upi)
                    (SfyDbu (yCoord (getSGq i xy)) upi) (getSGq i orient))))
        spins)
      refs n))
    (setq n (SfyEmit port "wire"
      (mapcar (lambda (f)
                (setq b (list (SfyWirePts (getSGq f points) upi) (SfyWidth f upi)))
                (list b f b))
        wires)
      refs n))
    ;; labels: key (x y text layer purpose orient justify font height kind); the parent
    ;; is written as its ref (0: none, or not a dumped object).
    (foreach e (SfySortByKey
                 (mapcar (lambda (f)
                           (list (list (SfyDbu (xCoord (getSGq f xy)) upi) (SfyDbu (yCoord (getSGq f xy)) upi)
                                   (getSGq f theLabel) (getSGq f layerName) (getSGq f purpose)
                                   (getSGq f orient) (getSGq f justify) (getSGq f font)
                                   (SfyDbu (getSGq f height) upi) (SfyLabelKind (getSGq f labelType)))
                             f))
                   labels))
      (setq n (add1 n))
      (setarray refs (cadr e) n)
      (setq par (getSGq (cadr e) parent))
      (setq b (car e))
      (SfyPut port "label"
        (list n (or (and par (arrayref refs par)) 0) (nth 2 b) (nth 0 b) (nth 1 b) (nth 5 b)
          (nth 6 b) (nth 7 b) (nth 8 b) (nth 9 b) (nth 3 b) (nth 4 b))))
    (setq n (SfyEmit port "shape"
      (mapcar (lambda (f)
                (list (list (SfyShapeCoords f upi) (getSGq f objType) (getSGq f layerName)
                        (getSGq f purpose) (SfyWidth f upi))
                  f
                  (list (getSGq f objType) (getSGq f layerName) (getSGq f purpose)
                    (SfyShapeCoords f upi) (SfyWidth f upi))))
        shapes)
      refs n))
    ;; append takes exactly two lists in Virtuoso (a live IC23/ICADVM20 run signalled on six).
    (SfyDumpProps port cv refs
      (append pinfigs (append insts (append spins (append wires (append labels shapes))))))))

;; A property's value as dumped (spec 4.2 prop), or nil for a type the dump skips.
(procedure (SfyPropValue p)
  (let ((v (getSGq p value)))
    (case (getSGq p valueType)
      ("string" (and (stringp v) v))
      ("int" (and (fixp v) (sprintf nil "%d" v)))
      ("float" (and (floatp v) (sprintf nil "%.17g" v)))
      ("boolean" (if (SfyYes v) "TRUE" "FALSE"))
      ("ILList" (sprintf nil "%L" v))
      ;; An inherited-connection override: the net name (dbReplaceProp ... "netSet" net).
      ("netSet" (and (stringp v) v))
      (t nil))))

;; prop records of every dumped object and of the cellview (ref 0; only properties named
;; schemify*, the rest being Virtuoso's bookkeeping), sorted by (ref name).
(procedure (SfyDumpProps port cv refs objs)
  (let ((entries nil) (v nil) (r nil))
    (foreach o (cons cv objs)
      (setq r (if (eq o cv) 0 (arrayref refs o)))
      (foreach p (getSGq o prop)
        (setq v (SfyPropValue p))
        (cond
          ((and (zerop r) (nequal (substring (getSGq p name) 1 8) "schemify")) nil)
          ((null v) (SfyCount (strcat "skipped:prop " (getSGq p valueType))))
          (t (setq entries (cons (list (list r (getSGq p name)) nil
                                   (list r (getSGq p name) (getSGq p valueType) v))
                             entries))))))
    (foreach e (SfySortByKey entries)
      (SfyPut port "prop" (caddr e)))))

;; textview records for the veriloga.va / verilog.v masters of the cell's other views.
(procedure (SfyDumpTextViews port lib cell)
  (let ((f nil) (p nil) (segs nil) (line nil))
    (foreach v (sort (getSGq (getSGq (ddGetObj lib cell) views) name) nil)
      (foreach file (list "veriloga.va" "verilog.v")
        (when (and (not (member v (list "schematic" "symbol"))) (setq f (ddGetObj lib cell v file)))
          (setq p (infile (getSGq f readPath)))
          (if (null p)
            (SfyRefuse 0 (sprintf nil "cannot read %s/%s/%s/%s" lib cell v file))
            (progn
              (setq segs nil)
              (while (gets line p) (setq segs (cons line segs)))
              (close p)
              (SfyPut port "textview" (list v file (SfyText (reverse segs)))))))))))

;; ================================================================ import

;; Loads path (spec 5.2): creates or (with overwrite) replaces each "design" cell's CDF,
;; views and text views in library lib (default: the record's library; it must be in
;; cds.lib); never touches "ref" cells. Writes <path>.log; t on PASS. A malformed or
;; truncated file is refused before anything is created.
(procedure (SfyImport sfyPath @key (lib nil) (callbacks nil) (check t) (overwrite nil))
  (let ((sfyLogPort (SfyLogOpen sfyPath)) (sfyCountTab (makeTable "sfyCounts" nil))
        (sfyCountKeys nil) (sfyFails 0) (sfyOverwrite overwrite) (sfyCallbacks callbacks)
        (sfyCheck check) (sfyTargets (makeTable "sfyTargets" nil))
        (sfyMasters nil) (sfyRead nil) (sfyGrouped nil))
    (SfyLog (sprintf nil "SfyImport %s" sfyPath))
    (SfyPreflight)
    (setq sfyRead (SfyReadRecords sfyPath))
    (when (car sfyRead) (setq sfyGrouped (SfyGroup (car sfyRead) lib)))
    (cond
      ((null (car sfyRead)) (SfyRefuse (cadr sfyRead) (caddr sfyRead)))
      ((null sfyGrouped) nil)
      (t
        (foreach sfyC (car sfyGrouped)
          (when (equal (arrayref sfyC "role") "design")
            (setarray sfyTargets (strcat (arrayref sfyC "rlib") "/" (arrayref sfyC "name")) (arrayref sfyC "lib"))))
        ;; Backstops: an unexpected SKILL error costs one cell, cellview or the maestro
        ;; part, logged with its message; a cellview it left open for writing is purged.
        (foreach sfyC (car sfyGrouped)
          (if (equal (arrayref sfyC "role") "design")
            (unless (errset (SfyLoadCellFirst sfyC) t)
              (SfyRefuse (arrayref sfyC "line") (sprintf nil "SKILL error %s" (SfyErrMsg)))
              (SfyPurgeWriting (arrayref sfyC "lib") (arrayref sfyC "name")))
            (SfyCount "skipped:ref cell")))
        (foreach sfyC (car sfyGrouped)
          (when (and (equal (arrayref sfyC "role") "design") (not (arrayref sfyC "skip")))
            (foreach sfyV (arrayref sfyC "cvs")
              (when (equal (arrayref sfyV "type") "schematic")
                (unless (errset (SfyLoadCv sfyC sfyV) t)
                  (SfyRefuse (arrayref sfyV "line") (sprintf nil "SKILL error %s" (SfyErrMsg)))
                  (SfyPurgeWriting (arrayref sfyC "lib") (arrayref sfyC "name")))))))
        (when (cadr sfyGrouped)
          (if (fboundp 'SfyAdeImport)
            (unless (errset (SfyAdeImport (cadr sfyGrouped) lib) t)
              (SfyRefuse 0 (sprintf nil "maestro: SKILL error %s" (SfyErrMsg))))
            (SfyCount "skipped:maestro")))))
    (foreach sfyE sfyMasters (dbClose (cadr sfyE)))
    (SfyLogClose)))

;; Groups records (line record) into cells: tables with "line" "lib" (target) "rlib"
;; (dumped) "name" "role" "cdf" (records) "cvs" "texts"; cellview tables with "line" "view"
;; "type" "upi" "recs". Returns (cells maestroRecords), or nil after refusing the first
;; record out of context.
(procedure (SfyGroup recs lib)
  (let ((cells nil) (cell nil) (cv nil) (rlib nil) (ade nil) (inAde nil) (err nil)
        (tag nil) (line nil))
    (foreach lr (cdr recs)
      (setq line (car lr))
      (setq tag (car (cadr lr)))
      (unless err
        (cond
          ((equal tag "end") nil)
          ((equal tag "lib")
            (setq rlib (cadr (cadr lr)))
            (setq cell nil)
            (setq cv nil)
            (setq inAde nil))
          ((equal tag "cell")
            (setq cv nil)
            (setq inAde nil)
            (if (null rlib)
              (setq err "cell before any lib record")
              (progn
                (setq cell (makeTable "sfyCell" nil))
                (setarray cell "line" line)
                (setarray cell "rlib" rlib)
                (setarray cell "lib" (or lib rlib))
                (setarray cell "name" (nth 1 (cadr lr)))
                (setarray cell "role" (nth 2 (cadr lr)))
                (unless (member (arrayref cell "role") (list "design" "ref"))
                  (setq err "cell role is neither design nor ref"))
                (setq cells (cons cell cells)))))
          ((member tag (list "cdf" "param" "siminfo"))
            (cond
              ((or (null cell) (arrayref cell "cvs") (arrayref cell "texts"))
                (setq err (strcat tag " outside a cell's CDF section")))
              ((and (nequal tag "cdf") (null (arrayref cell "cdf")))
                (setq err (strcat tag " before its cdf record")))
              ((and (equal tag "cdf") (arrayref cell "cdf"))
                (setq err "a second cdf record in one cell"))
              (t (setarray cell "cdf" (cons lr (arrayref cell "cdf"))))))
          ((equal tag "cv")
            (if (null cell)
              (setq err "cv outside a cell")
              (progn
                (setq cv (makeTable "sfyCv" nil))
                (setarray cv "line" line)
                (setarray cv "view" (nth 1 (cadr lr)))
                (setarray cv "type" (nth 2 (cadr lr)))
                (setarray cv "upi" (nth 3 (cadr lr)))
                (unless (member (arrayref cv "type") (list "schematic" "schematicSymbol"))
                  (setq err "cv view type is neither schematic nor schematicSymbol"))
                (unless (greaterp (arrayref cv "upi") 0)
                  (setq err "cv units per inch must be positive"))
                (setarray cell "cvs" (cons cv (arrayref cell "cvs"))))))
          ((equal tag "textview")
            (if (null cell)
              (setq err "textview outside a cell")
              (progn
                (setq cv nil)
                (setarray cell "texts" (cons lr (arrayref cell "texts"))))))
          ((equal tag "maestro")
            (setq inAde t)
            (setq cell nil)
            (setq cv nil)
            (setq ade (cons lr ade)))
          ((member tag (list "var" "analysis" "corner" "model" "output" "spec" "defn"))
            (if inAde (setq ade (cons lr ade)) (setq err (strcat tag " outside a maestro test"))))
          ((null cv) (setq err (strcat tag " outside a cellview")))
          (t (setarray cv "recs" (cons lr (arrayref cv "recs"))))))
      (when (and err (not (equal err t)))
        (SfyRefuse line err)
        (setq err t)))
    (unless err
      (foreach c cells
        (setarray c "cdf" (reverse (arrayref c "cdf")))
        (setarray c "texts" (reverse (arrayref c "texts")))
        (setarray c "cvs" (reverse (arrayref c "cvs")))
        (foreach v (arrayref c "cvs") (setarray v "recs" (reverse (arrayref v "recs")))))
      (list (reverse cells) (reverse ade)))))

;; First pass over a design cell: its CDF, its symbol (and other non-schematic) views and
;; its text views, so that the schematics of every cell find their masters.
(procedure (SfyLoadCellFirst c)
  (if (null (ddGetObj (arrayref c "lib")))
    (progn
      (SfyRefuse (arrayref c "line") (sprintf nil "library %s is not in cds.lib" (arrayref c "lib")))
      (setarray c "skip" t))
    (progn
      ;; A kept symbol (spec 7.5) keeps its CDF too: the CDF is the symbol part's.
      (cond
        ((SfyKeptP c "symbol") (SfyCount "kept unchanged CDF"))
        ((arrayref c "cdf") (SfyLoadCdf c)))
      (foreach v (arrayref c "cvs")
        (unless (equal (arrayref v "type") "schematic") (SfyLoadCv c v)))
      (unless (SfyKeptP c "symbol") (SfyLoadStopViews c))
      (foreach lr (arrayref c "texts") (SfyLoadText c lr)))))

;; A primitive (simInfo, no schematic in the dump) gets its symbol a second time as the
;; stopping view of each netlister it has simInfo for, spectre and auCdl, as analogLib cells
;; have: without one the netlister descends into the cell and fails (OSSHNL-116 on a live
;; ICADVM20.1 run). The dumper reads only symbol and schematic, so these never travel back.
(procedure (SfyLoadStopViews c)
  (let ((sfySym (car (setof sfyV (arrayref c "cvs") (equal (arrayref sfyV "view") "symbol"))))
        (sfySims nil))
    (foreach sfyLr (arrayref c "cdf")
      (when (and (equal (car (cadr sfyLr)) "siminfo")
                 (member (nth 1 (cadr sfyLr)) (list "spectre" "auCdl")))
        (setq sfySims (cons (nth 1 (cadr sfyLr)) sfySims))))
    (when (and sfySym
               (not (exists sfyV (arrayref c "cvs") (equal (arrayref sfyV "type") "schematic"))))
      (foreach sfySim (reverse sfySims)
        (if (and (ddGetObj (arrayref c "lib") (arrayref c "name") sfySim) (not sfyOverwrite))
          (SfyCount "kept stop view")
          (progn
            (setarray sfySym "view" sfySim)
            (SfyLoadCv c sfySym)
            (setarray sfySym "view" "symbol")))))))

;; True when the dump marks cellview `view` of cell c schemifyKeep (unchanged in Schemify
;; since it was imported, spec 7.5) and the library has that cellview: the loader leaves it,
;; and for a symbol its CDF, exactly as they are.
(procedure (SfyKeptP c view)
  (exists v (arrayref c "cvs")
    (and (equal (arrayref v "view") view)
         (ddGetObj (arrayref c "lib") (arrayref c "name") view)
         (exists lr (arrayref v "recs")
           (and (equal (car (cadr lr)) "prop") (equal (nth 1 (cadr lr)) 0)
                (equal (nth 2 (cadr lr)) "schemifyKeep"))))))

;; ---------------------------------------------------------------- CDF

(procedure (SfyLoadCdf c)
  (let ((tlib (arrayref c "lib")) (name (arrayref c "name")) (cellId nil) (old nil)
        (cdf nil) (sim nil) (line (car (car (arrayref c "cdf")))))
    (setq cellId (ddGetObj tlib name nil nil nil "a"))
    (when cellId (setq old (cdfGetBaseCellCDF cellId)))
    (cond
      ((null cellId) (SfyRefuse line (sprintf nil "cannot create cell %s/%s" tlib name)))
      ((and old (not sfyOverwrite))
        (SfyRefuse line (sprintf nil "%s/%s has a base CDF; pass ?overwrite t" tlib name)))
      (t
        (when old (cdfDeleteCDF old))
        (setq cdf (cdfCreateBaseCellCDF cellId))
        (if (null cdf)
          (SfyRefuse line (sprintf nil "cannot create the base CDF of %s/%s" tlib name))
          (progn
            (foreach lr (arrayref c "cdf")
              (case (car (cadr lr))
                ("cdf"
                  (when (nequal (SfyTextValue (cadr (cadr lr))) "")
                    (setSGq cdf (SfyTextValue (cadr (cadr lr))) paramLabelSet)))
                ("param" (SfyLoadParam cdf lr))
                ("siminfo" (setq sim (cons (SfySimInfoValue (cadr lr)) sim)))))
            (when sim (setSGq cdf (cons nil (SfyFlatten (reverse sim))) simInfo))
            (if (cdfSaveCDF cdf)
              (SfyCount "cdf")
              (SfyRefuse line (sprintf nil "cannot save the base CDF of %s/%s" tlib name)))))))))

;; ((a b) (c d)) -> (a b c d)
(procedure (SfyFlatten pairs)
  (let ((out nil))
    (foreach p pairs (setq out (cons (cadr p) (cons (car p) out))))
    (reverse out)))

;; A simInfo field value back: netlistProcedure as a symbol, other strings as strings,
;; pairs as a property list of symbols (strings starting with "(" stay strings), other
;; lists as symbols.
(procedure (SfySimSym s)
  (if (equal (substring s 1 1) "(") s (stringToSymbol s)))

(procedure (SfySimInfoValue rec)
  ;; ("siminfo" simulator fields) -> (simulatorSymbol (nil key value ...))
  (let ((out nil) (v nil))
    (foreach f (caddr rec)
      (setq v (cadr f))
      (setq out (cons (stringToSymbol (car f)) out))
      (setq out (cons
        (cond
          ((stringp v) (if (equal (car f) "netlistProcedure") (stringToSymbol v) v))
          ((null v) nil)
          ((listp (car v)) (cons nil (SfyFlatten (mapcar (lambda (p) (list (SfySimSym (car p)) (SfySimSym (cadr p)))) v))))
          (t (mapcar 'SfySimSym v)))
        out)))
    (list (stringToSymbol (cadr rec)) (cons nil (reverse out)))))

;; A CDF value of type ty from its text (int, float, boolean; the rest stay strings).
(procedure (SfyCdfValue ty s)
  (case ty
    ("int" (atoi s))
    ("float" (atof s))
    ("boolean" (SfyYes s))
    (t s)))

;; A CDF value as compared and logged: booleans "TRUE"/"FALSE", the rest via SfyAtomStr.
(procedure (SfyCdfStr ty v)
  (if (equal ty "boolean") (if (SfyYes v) "TRUE" "FALSE") (SfyAtomStr v)))

;; ("param" name type defValue prompt units flags choices callback) -> cdfCreateParam.
(procedure (SfyLoadParam cdf lr)
  (let ((f (cdr (cadr lr))) (args nil) (ty nil) (def nil))
    (setq ty (nth 1 f))
    (setq args (list cdf '?name (nth 0 f) '?type ty '?prompt (nth 3 f)))
    (unless (equal ty "button")
      (setq def (if (equal ty "boolean") (equal (nth 2 f) "t") (SfyCdfValue ty (nth 2 f))))
      (setq args (append args (list '?defValue def))))
    (unless (equal (nth 4 f) "") (setq args (append args (list '?units (nth 4 f)))))
    (when (nth 6 f) (setq args (append args (list '?choices (nth 6 f)))))
    (when (member "parseAsNumber" (nth 5 f)) (setq args (append args (list '?parseAsNumber "yes"))))
    (when (member "parseAsCEL" (nth 5 f)) (setq args (append args (list '?parseAsCEL "yes"))))
    (when (member "storeDefault" (nth 5 f)) (setq args (append args (list '?storeDefault "yes"))))
    (when (member "dontSave" (nth 5 f)) (setq args (append args (list '?dontSave "t"))))
    (when (member "hidden" (nth 5 f)) (setq args (append args (list '?display "nil"))))
    (unless (equal (SfyTextValue (nth 7 f)) "")
      (setq args (append args (list '?callback (SfyTextValue (nth 7 f))))))
    (cond
      ((and (member ty (list "int" "float")) (null def))
        (SfyRefuse (car lr) (sprintf nil "param %s: default %s is not a number" (nth 0 f) (nth 2 f))))
      ((apply 'cdfCreateParam args) (SfyCount "param"))
      (t (SfyRefuse (car lr) (sprintf nil "cannot create CDF parameter %s" (nth 0 f)))))))

;; ---------------------------------------------------------------- cellviews

;; Creates (or with overwrite replaces) one cellview from its records; a schematic is
;; schCheck'ed and saved only without check errors (unless ?check nil).
(procedure (SfyLoadCv sfyC sfyV)
  (let ((sfyTlib (arrayref sfyC "lib")) (sfyCell (arrayref sfyC "name")) (sfyView (arrayref sfyV "view"))
        (sfyLine (arrayref sfyV "line")) (sfyCv nil) (sfyUpi (arrayref sfyV "upi"))
        (sfyRefs (makeTable "sfyObjs" nil)) (sfyTerms (makeTable "sfyTerms" nil))
        (sfyProps nil) (sfyConns nil) (sfyTag nil)
        (sfyKeep (exists sfyLr (arrayref sfyV "recs")
                (and (equal (car (cadr sfyLr)) "prop") (equal (nth 1 (cadr sfyLr)) 0)
                     (equal (nth 2 (cadr sfyLr)) "schemifyKeep")))))
    (cond
      ;; Unchanged in Schemify since it was imported from here (spec 7.5): the cellview in
      ;; the library stays exactly as it is.
      ((and sfyKeep (ddGetObj sfyTlib sfyCell sfyView))
        (SfyCount "kept unchanged"))
      ((and (ddGetObj sfyTlib sfyCell sfyView) (not sfyOverwrite))
        (SfyRefuse sfyLine (sprintf nil "%s/%s/%s exists; pass ?overwrite t" sfyTlib sfyCell sfyView)))
      ((SfyOpenForEdit sfyTlib sfyCell sfyView)
        (SfyRefuse sfyLine (sprintf nil "%s/%s/%s is open for editing in Virtuoso; close it first"
                          sfyTlib sfyCell sfyView)))
      ((progn (printf "schemify: loading %s/%s/%s\n" sfyTlib sfyCell sfyView) nil) nil)
      ((null (setq sfyCv (dbOpenCellViewByType sfyTlib sfyCell sfyView (arrayref sfyV "type") "w")))
        (SfyRefuse sfyLine (sprintf nil "cannot open %s/%s/%s for writing" sfyTlib sfyCell sfyView)))
      (t
        (setarray sfyRefs 0 sfyCv)
        (foreach sfyLr (arrayref sfyV "recs")
          (setq sfyTag (car (cadr sfyLr)))
          (case sfyTag
            ("term" (SfyLoadTerm sfyLr))
            ("pinfig" (SfyLoadPinFig sfyLr))
            ("inst" (SfyLoadInst sfyLr))
            ("spin" (SfyLoadSpin sfyLr))
            ("wire" (SfyLoadWire sfyLr))
            ("label" (SfyLoadLabel sfyLr))
            ("shape" (SfyLoadShape sfyLr))
            ("prop" (unless (equal (nth 2 (cadr sfyLr)) "schemifyKeep") (setq sfyProps (cons sfyLr sfyProps))))
            ("conn" (setq sfyConns (cons sfyLr sfyConns)))
            (t (SfyCount (strcat "skipped:" sfyTag)))))
        (SfyLoadProps (reverse sfyProps))
        (foreach sfyLr (reverse sfyConns) (SfyLoadConn sfyLr))
        (SfyFinishCv sfyTlib sfyCell sfyView sfyLine)))))

;; Purges every cellview of lib/cell still open for writing ("w"): what an import left
;; behind when a SKILL error cut it short, so no half-written cellview stays open or locked.
(procedure (SfyPurgeWriting lib cell)
  (foreach cv (dbGetOpenCellViews)
    (when (and (equal (getSGq cv libName) lib) (equal (getSGq cv cellName) cell)
               (equal (getSGq cv mode) "w"))
      (dbPurge cv))))

;; t when lib/cell/view is open other than read-only (an editor window): reopening it "w"
;; would rewrite the cellview under that window.
(procedure (SfyOpenForEdit lib cell view)
  (exists cv (dbGetOpenCellViews)
    (and (equal (getSGq cv libName) lib) (equal (getSGq cv cellName) cell)
         (equal (getSGq cv viewName) view) (nequal (getSGq cv mode) "r"))))

;; Checks (schematics) and saves the open cellview sfyCv, or purges it.
(procedure (SfyFinishCv tlib cell view line)
  (let ((res nil) (errors 0))
    (when (equal (getSGq sfyCv cellViewType) "schematic")
      (setq res (schCheck sfyCv))
      (setq errors (or (car res) 0))
      (SfyLog (sprintf nil "check %s/%s/%s: %d errors %d warnings" tlib cell view errors
                (or (cadr res) 0))))
    (cond
      ((and sfyCheck (greaterp errors 0))
        (SfyRefuse line (sprintf nil "%s/%s/%s not saved: %d check errors (?check nil saves anyway)"
                          tlib cell view errors))
        (dbPurge sfyCv))
      ((dbSave sfyCv)
        (SfyCount (strcat "saved " (getSGq sfyCv cellViewType)))
        (dbClose sfyCv))
      (t
        (SfyRefuse line (sprintf nil "cannot save %s/%s/%s" tlib cell view))
        (dbPurge sfyCv)))))

;; Database units -> user units (spec 5.1).
(procedure (SfyUU v)
  (quotient (float v) sfyUpi))

(procedure (SfyPt x y)
  (list (SfyUU x) (SfyUU y)))

;; Flat (x1 y1 x2 y2 ...) -> ((x1 y1) (x2 y2) ...) in user units.
(procedure (SfyPts flat)
  (let ((out nil))
    (while flat
      (setq out (cons (SfyPt (car flat) (cadr flat)) out))
      (setq flat (cddr flat)))
    (reverse out)))

;; The created object of ref r, or nil after refusing record lr.
;; The object created for ref r; nil (refused) when none was. A retarget placeholder's ref
;; (spec 10.1) answers nil without a refusal: SfyRetarget places it later.
(procedure (SfyRef r lr)
  (let ((o (arrayref sfyRefs r)))
    (cond
      ((equal o "sfyPlaceholder") (SfyCount "deferred to SfyRetarget") nil)
      ((null o) (SfyRefuse (car lr) (sprintf nil "%s: ref %d names no created object" (car (cadr lr)) r)) nil)
      (t o))))

;; Records object o under the ref of record lr and counts it; refuses when o is nil.
(procedure (SfyCreated o lr what)
  (if o
    (progn
      (setarray sfyRefs (cadr (cadr lr)) o)
      (SfyCount (car (cadr lr))))
    (SfyRefuse (car lr) (sprintf nil "%s failed" what))))

;; The master cellview lib/cell/view, opened read-only once per import (SfyImport closes
;; it); a design cell of the dump resolves to its target library.
(procedure (SfyMaster lib cell view)
  (let ((t0 (arrayref sfyTargets (strcat lib "/" cell))) (k nil) (m nil))
    (when t0 (setq lib t0))
    (setq k (strcat lib "/" cell "/" view))
    ;; ponytail: a linear assoc over the masters of one import (tens to hundreds); a table
    ;; when an import opens thousands.
    (setq m (cadr (assoc k sfyMasters)))
    (unless m
      (setq m (dbOpenCellViewByType lib cell view nil "r"))
      (when m (setq sfyMasters (cons (list k m) sfyMasters))))
    m))

(procedure (SfySchematicP lr)
  (if (equal (getSGq sfyCv cellViewType) "schematic")
    t
    (progn
      (SfyRefuse (car lr) (sprintf nil "%s records load into schematics only" (car (cadr lr))))
      nil)))

(procedure (SfySymbolP lr)
  (if (equal (getSGq sfyCv cellViewType) "schematic")
    (progn
      (SfyRefuse (car lr) (sprintf nil "%s records load into symbols only" (car (cadr lr))))
      nil)
    t))

;; ("term" name dir netexpr) in a symbol: net, terminal, net expression.
(procedure (SfyLoadTerm lr)
  (let ((f (cdr (cadr lr))) (net nil) (term nil))
    (when (SfySymbolP lr)
      (setq net (dbMakeNet sfyCv (nth 0 f)))
      (when net (setq term (dbCreateTerm net (nth 0 f) (nth 1 f))))
      (cond
        ((null term) (SfyRefuse (car lr) (sprintf nil "cannot create terminal %s" (nth 0 f))))
        ((and (nequal (nth 2 f) "") (not (dbCreateTermNetExpr term (nth 2 f))))
          (SfyRefuse (car lr) (sprintf nil "cannot set net expression %s on %s" (nth 2 f) (nth 0 f))))
        (t
          (setarray sfyTerms (nth 0 f) term)
          (SfyCount "term"))))))

;; ("pinfig" ref term x1 y1 x2 y2): a pin rectangle of the terminal.
(procedure (SfyLoadPinFig lr)
  (let ((f (cdr (cadr lr))) (term nil) (rect nil))
    (when (SfySymbolP lr)
      (setq term (arrayref sfyTerms (nth 1 f)))
      (if (null term)
        (SfyRefuse (car lr) (sprintf nil "pinfig of unknown terminal %s" (nth 1 f)))
        (progn
          (setq rect (dbCreateRect sfyCv (list "pin" "drawing")
                       (list (SfyPt (nth 2 f) (nth 3 f)) (SfyPt (nth 4 f) (nth 5 f)))))
          (when (and rect (not (dbCreatePin (getSGq term net) rect nil term)))
            (setq rect nil))
          (SfyCreated rect lr "pin figure"))))))

;; ("inst" ref name lib cell view x y orient)
(procedure (SfyLoadInst lr)
  (let ((f (cdr (cadr lr))) (m nil))
    (when (and (SfySchematicP lr) (equal (nth 2 f) ""))
      ;; A retarget placeholder (library ""): its master is chosen on the target machine by
      ;; SfyRetarget (spec 10.1); it and its props and conn records are skipped here.
      (setarray sfyRefs (nth 0 f) "sfyPlaceholder")
      (SfyCount "placeholder"))
    (when (and (SfySchematicP lr) (nequal (nth 2 f) ""))
      (setq m (SfyMaster (nth 2 f) (nth 3 f) (nth 4 f)))
      (if (null m)
        (SfyRefuse (car lr) (sprintf nil "no master %s/%s/%s" (nth 2 f) (nth 3 f) (nth 4 f)))
        (SfyCreated (schCreateInst sfyCv m (nth 1 f) (SfyPt (nth 5 f) (nth 6 f)) (nth 7 f))
          lr (sprintf nil "instance %s" (nth 1 f)))))))

;; ("spin" ref term dir lib cell view x y orient): a schematic pin.
(procedure (SfyLoadSpin lr)
  (let ((f (cdr (cadr lr))) (m nil) (p nil))
    (when (SfySchematicP lr)
      (setq m (SfyMaster (nth 3 f) (nth 4 f) (nth 5 f)))
      (if (null m)
        (SfyRefuse (car lr) (sprintf nil "no master %s/%s/%s" (nth 3 f) (nth 4 f) (nth 5 f)))
        (progn
          (setq p (schCreatePin sfyCv m (nth 1 f) (nth 2 f) nil (SfyPt (nth 6 f) (nth 7 f)) (nth 8 f)))
          ;; GUESS(G11): schCreatePin returns the pin instance; a pin object gives its fig.
          (when (and p (equal (getSGq p objType) "pin")) (setq p (SfyPinFig p)))
          (SfyCreated p lr (sprintf nil "schematic pin %s" (nth 1 f))))))))

;; ("wire" ref points width): schCreateWire "draw", snapped to one database unit.
(procedure (SfyLoadWire lr)
  (let ((f (cdr (cadr lr))) (snap (quotient 1.0 sfyUpi)))
    (when (SfySchematicP lr)
      (SfyCreated
        (car (schCreateWire sfyCv "draw" "full" (SfyPts (nth 1 f)) snap snap (SfyUU (nth 2 f))))
        lr "wire"))))

;; ("label" ref parent text x y orient justify font height kind layer purpose)
(procedure (SfyLoadLabel lr)
  (let ((f (cdr (cadr lr))) (par nil) (pt nil) (h nil) (lt nil) (lbl nil) (lpp nil) (wireP nil))
    (setq par (if (zerop (nth 1 f)) nil (SfyRef (nth 1 f) lr)))
    (when (or par (zerop (nth 1 f)))
      (setq pt (SfyPt (nth 3 f) (nth 4 f)))
      (setq h (SfyUU (nth 8 f)))
      (setq lt (case (nth 9 f) ("NLP" "NLPLabel") ("IL" "ILLabel") (t "normalLabel")))
      (setq lpp (list (nth 10 f) (nth 11 f)))
      (setq wireP (and par (member (getSGq par objType) (list "line" "path"))
                    (equal (getSGq par layerName) "wire")))
      (setq lbl
        (cond
          ((and wireP (equal lpp (list "wire" "label")) (equal lt "NLPLabel")
                (equal (substring (nth 2 f) 1 2) "[@"))
            (schCreateNetExpression sfyCv (nth 2 f) par pt (nth 6 f) (nth 5 f) (nth 7 f) h))
          ((and wireP (equal lpp (list "wire" "label")) (equal lt "normalLabel"))
            (schCreateWireLabel sfyCv par pt (nth 2 f) (nth 6 f) (nth 5 f) (nth 7 f) h nil))
          ((and wireP (equal lpp (list "annotate" "drawing3")) (equal lt "normalLabel"))
            (schCreateWireLabel sfyCv par pt (nth 2 f) (nth 6 f) (nth 5 f) (nth 7 f) h t))
          ((and (null par) (equal lpp (list "text" "drawing")))
            (schCreateNoteLabel sfyCv pt (nth 2 f) (nth 6 f) (nth 5 f) (nth 7 f) h lt))
          (t
            (setq lbl (dbCreateLabel sfyCv lpp pt (nth 2 f) (nth 6 f) (nth 5 f) (nth 7 f) h))
            (when (and lbl (nequal lt "normalLabel")) (setSGq lbl lt labelType))
            ;; research U13: schGlueLabel first, the parent attribute as the fallback.
            (when (and lbl par (not (car (errset (schGlueLabel lbl par)))))
              (setSGq lbl par parent))
            lbl)))
      (SfyCreated lbl lr (sprintf nil "label %s" (nth 2 f))))))

;; ("shape" ref kind layer purpose coords width)
(procedure (SfyLoadShape lr)
  (let ((f (cdr (cadr lr))) (lpp nil) (c nil) (s nil))
    (setq lpp (list (nth 2 f) (nth 3 f)))
    (setq c (nth 4 f))
    (setq s
      (case (nth 1 f)
        ("line" (dbCreateLine sfyCv lpp (SfyPts c)))
        ("polygon" (dbCreatePolygon sfyCv lpp (SfyPts c)))
        ("path" (dbCreatePath sfyCv lpp (SfyPts c) (SfyUU (nth 5 f))))
        ("rect" (dbCreateRect sfyCv lpp (SfyPts c)))
        ("ellipse" (dbCreateEllipse sfyCv lpp (SfyPts c)))
        ("arc"
          ;; GUESS(G10): an arc is created on its ellipse box, then its angles are set.
          ;; coords (x1 y1 x2 y2 a0 a1): the ellipse box, then the angles in micro-radians.
          (setq s (dbCreateArc sfyCv lpp (SfyPts (list (nth 0 c) (nth 1 c) (nth 2 c) (nth 3 c)))
                    (SfyPts (list (nth 0 c) (nth 1 c) (nth 2 c) (nth 3 c)))))
          (when s
            (setSGq s (quotient (float (nth 4 c)) 1000000.0) startAngle)
            (setSGq s (quotient (float (nth 5 c)) 1000000.0) stopAngle))
          s)
        (t nil)))
    (SfyCreated s lr (sprintf nil "%s shape" (nth 1 f)))))

;; ---------------------------------------------------------------- properties

;; prop records (sorted by ref), one group per object: an instance with a CDF takes its
;; CDF parameters through the instance CDF (spec 5.3), everything else dbReplaceProp.
(procedure (SfyLoadProps sfyProps)
  (let ((sfyGroups nil) (sfyCur nil) (sfyCurRef nil))
    (foreach sfyLr sfyProps
      (unless (equal (cadr (cadr sfyLr)) sfyCurRef)
        (when sfyCur (setq sfyGroups (cons (reverse sfyCur) sfyGroups)))
        (setq sfyCur nil)
        (setq sfyCurRef (cadr (cadr sfyLr))))
      (setq sfyCur (cons sfyLr sfyCur)))
    (when sfyCur (setq sfyGroups (cons (reverse sfyCur) sfyGroups)))
    (foreach sfyG (reverse sfyGroups)
      (SfyLoadPropGroup sfyG))))

(procedure (SfyLoadPropGroup sfyG)
  (let ((sfyO (SfyRef (cadr (cadr (car sfyG))) (car sfyG))) (sfyCdf nil))
    (when (and sfyO (nequal sfyO sfyCv) (equal (getSGq sfyO objType) "inst"))
      (setq sfyCdf (cdfGetInstCDF sfyO)))
    (cond
      ((null sfyO) nil)
      (sfyCdf (SfyLoadParams sfyO sfyCdf sfyG))
      (t (foreach sfyLr sfyG (SfyReplaceProp sfyO sfyLr))))))

;; ("prop" ref name type value) through dbReplaceProp.
(procedure (SfyReplaceProp o lr)
  (let ((f (cdr (cadr lr))) (v nil) (ok t))
    (setq v (nth 3 f))
    (case (nth 2 f)
      (("string" "netSet") nil)
      ("int" (setq v (atoi v)) (setq ok (fixp v)))
      ("float" (setq v (atof v)) (setq ok (floatp v)))
      ("boolean" (setq v (SfyYes v)))
      ("ILList" (setq v (errset (linereadstring v))) (setq ok v) (setq v (car v)))
      (t (setq ok nil)))
    (cond
      ((not ok) (SfyRefuse (car lr) (sprintf nil "property %s: %s value %s" (nth 1 f) (nth 2 f) (nth 3 f))))
      ((dbReplaceProp o (nth 1 f) (nth 2 f) v) (SfyCount "prop"))
      (t (SfyRefuse (car lr) (sprintf nil "cannot set property %s" (nth 1 f)))))))

;; Sets an instance's CDF parameters in CDF order (spec 5.3): a value equal to the CDF
;; default is not set unless the parameter has storeDefault. With ?callbacks t a parameter
;; the PDK locks (editable "nil": derived from others, as nf from fingers) is left to the
;; callbacks, and once every value is set the callbacks of the set parameters run
;; (SfyCallbacks). Afterwards every set parameter is read back and a difference is logged
;; as clamped, and every locked one the file held is read back and a difference from the
;; file logged as derived (a callback that did not derive it shows here).
;; Locals and parameters of every procedure active while a PDK callback runs carry the sfy
;; prefix: SKILL scope is dynamic, and a callback that sets an undeclared w, p or inst
;; must not reach ours (tests/skill_lint.rs checks it).
(procedure (SfyLoadParams sfyInst sfyCdf sfyG)
  (let ((sfySent nil) (sfyDone nil) (sfyLocked nil) (sfyLr nil) (sfyTy nil) (sfyV nil)
        (sfyNow nil) (sfyNames nil) (sfyBad nil) (sfyFresh nil) (sfyQ nil))
    (foreach sfyR sfyG
      (if (cdfFindParamByName sfyCdf (nth 2 (cadr sfyR)))
        (setq sfySent (cons sfyR sfySent))
        (SfyReplaceProp sfyInst sfyR)))
    (foreach sfyP (getSGq sfyCdf parameters)
      (setq sfyLr (car (setof sfyR sfySent (equal (nth 2 (cadr sfyR)) (getSGq sfyP name)))))
      (when sfyLr
        (setq sfyTy (getSGq sfyP paramType))
        (setq sfyV (SfyCdfValue sfyTy (nth 4 (cadr sfyLr))))
        (cond
          ((and (member sfyTy (list "int" "float")) (null sfyV))
            (SfyRefuse (car sfyLr) (sprintf nil "parameter %s: %s is not a number" (getSGq sfyP name) (nth 4 (cadr sfyLr)))))
          ((and sfyCallbacks (equal (getSGq sfyP editable) "nil"))
            (SfyCount "skipped:read-only param")
            (setq sfyLocked (cons (list (getSGq sfyP name) (SfyCdfStr sfyTy sfyV) sfyLr) sfyLocked)))
          ((and (equal (SfyCdfStr sfyTy sfyV) (SfyCdfStr sfyTy (getSGq sfyP defValue)))
                (not (SfyYes (getSGq sfyP storeDefault))))
            (SfyCount "default"))
          (t
            (setSGq sfyP sfyV value)
            (SfyCount "param")
            (setq sfyDone (cons (list (getSGq sfyP name) (SfyCdfStr sfyTy sfyV) sfyLr) sfyDone))
            (when (SfyHasCallback sfyP)
              (if sfyCallbacks
                (setq sfyNames (cons (getSGq sfyP name) sfyNames))
                (SfyCount "callback not run")))))))
    (when sfyNames
      (printf "schemify: CDF callbacks of %s\n" (getSGq sfyInst name))
      (setq sfyBad (SfyCallbacks sfyInst (reverse sfyNames)))
      (foreach sfyN (reverse sfyNames)
        (if (member sfyN sfyBad)
          (progn
            (SfyCount "callback failed")
            (SfyLog (sprintf nil "callback failed line %d: %s %s: %s"
                      (car (caddr (assoc sfyN sfyDone))) (getSGq sfyInst name) sfyN
                      (getSGq (cdfFindParamByName sfyCdf sfyN) callback))))
          (SfyCount "callback"))))
    (setq sfyFresh (cdfGetInstCDF sfyInst))
    (foreach sfyE (reverse sfyDone)
      (setq sfyQ (cdfFindParamByName sfyFresh (car sfyE)))
      (setq sfyNow (SfyCdfStr (getSGq sfyQ paramType) (getSGq sfyQ value)))
      (unless (equal sfyNow (cadr sfyE))
        (SfyCount "clamped")
        (SfyLog (sprintf nil "clamped line %d: %s %s sent %s now %s" (car (caddr sfyE))
                  (getSGq sfyInst name) (car sfyE) (cadr sfyE) sfyNow))))
    (foreach sfyE (reverse sfyLocked)
      (setq sfyQ (cdfFindParamByName sfyFresh (car sfyE)))
      (setq sfyNow (SfyCdfStr (getSGq sfyQ paramType) (getSGq sfyQ value)))
      (unless (equal sfyNow (cadr sfyE))
        (SfyCount "derived")
        (SfyLog (sprintf nil "derived line %d: %s %s file %s now %s" (car (caddr sfyE))
                  (getSGq sfyInst name) (car sfyE) (cadr sfyE) sfyNow))))))

(procedure (SfyHasCallback sfyP)
  (and (stringp (getSGq sfyP callback)) (nequal (getSGq sfyP callback) "")))

;; Runs the CDF callbacks of the parameters sfyNames of instance sfyInst, in CDF order, the
;; way the Edit Properties form does (the sequence virtuoso-bridge-lite runs on live PDKs):
;; the master's cell CDF, loaded with the instance's values, is cdfgData and cdfgForm (so
;; cdfgData->id is the cell, as PDK callbacks expect) through the callbacks and
;; cdfUpdateInstParam, which writes the result (and doneProc's) to the instance; the cell
;; CDF then gets its own values back, whatever happened. Returns the names whose callback
;; failed (all of them when the master has no cell CDF or the update fails).
(procedure (SfyCallbacks sfyInst sfyNames)
  (let ((sfyIcdf (cdfGetInstCDF sfyInst)) (sfyCcdf nil) (sfySaved nil) (sfyIp nil)
        (sfyBad nil) (cdfgData nil) (cdfgForm nil))
    (setq sfyCcdf (car (errset (cdfGetCellCDF (ddGetObj (getSGq sfyInst libName)
                                                        (getSGq sfyInst cellName))))))
    (if (null sfyCcdf)
      sfyNames
      (progn
        (foreach sfyP (getSGq sfyCcdf parameters)
          (setq sfySaved (cons (list sfyP (getSGq sfyP value)) sfySaved)))
        (setq cdfgData sfyCcdf)
        (setq cdfgForm sfyCcdf)
        (unwindProtect
          (progn
            (foreach sfyP (getSGq sfyCcdf parameters)
              (setq sfyIp (cdfFindParamByName sfyIcdf (getSGq sfyP name)))
              (when sfyIp (setSGq sfyP (getSGq sfyIp value) value)))
            (foreach sfyP (getSGq sfyCcdf parameters)
              (when (and (member (getSGq sfyP name) sfyNames)
                         (not (SfyEvalCallback (getSGq sfyP callback))))
                (setq sfyBad (cons (getSGq sfyP name) sfyBad))))
            ;; Without the update nothing the callbacks did reaches the instance.
            (unless (errset (cdfUpdateInstParam sfyInst) t)
              (setq sfyBad (reverse sfyNames))))
          ;; No errset here: one that completes would clear the message of an error
          ;; on its way out to the import's backstop.
          (foreach sfyX sfySaved (setSGq (car sfyX) (cadr sfyX) value)))
        (reverse sfyBad)))))

;; Runs a PDK callback string inside errset under the caller's cdfgData and cdfgForm; t
;; when it ran without error. This is the only evalstring in the file, and it evaluates
;; PDK code, never file data.
(procedure (SfyEvalCallback sfyCb)
  (if (errset (evalstring sfyCb) t) t nil))

;; ---------------------------------------------------------------- stubs, text views

(procedure (SfyCenter b)
  (list (quotient (plus (xCoord (lowerLeft b)) (xCoord (upperRight b))) 2.0)
        (quotient (plus (yCoord (lowerLeft b)) (yCoord (upperRight b))) 2.0)))

;; ("conn" ref term net): a one-step stub from the terminal's real pin, away from the
;; instance, labelled with the net (spec 6.4); the stub carries schemifyStub "1".
(procedure (SfyLoadConn lr)
  (let ((f (cdr (cadr lr))) (inst nil) (why nil))
    (setq inst (SfyRef (nth 0 f) lr))
    (when inst
      (setq why (SfyStubAt sfyCv inst (nth 1 f) (nth 2 f)))
      (if (stringp why)
        (SfyRefuse (car lr) why)
        (SfyCount "conn")))))

;; Draws the stub + label for terminal term of instance inst in cellview cv: one grid step
;; (10 DBU of cv, 1/16 inch at the standard 160) from the terminal's real pin, away from the
;; instance's centre, the wire marked schemifyStub "1", the label glued at its far end.
;; Returns t, or a string saying what failed. Shared by the loader's conn records and by
;; SfyPlaceConn (the retarget flow).
(procedure (SfyStubAt cv inst term net)
  (prog (t0 fig p c dx dy q step w dbu)
    (setq dbu (getSGq cv DBUPerUU))
    (setq step (quotient 10.0 dbu))
    (when (equal (getSGq inst objType) "inst")
      (setq t0 (or (dbFindTermByName (getSGq inst master) term)
                   ;; Kits differ in case (D in one, d in another).
                   (car (setof x (getSGq (getSGq inst master) terminals)
                          (equal (lowerCase (getSGq x name)) (lowerCase term)))))))
    (when t0 (setq fig (SfyPinFig (car (getSGq t0 pins)))))
    (unless fig
      (return (sprintf nil "conn: no pin %s on %s" term (getSGq inst name))))
    (setq p (SfyCenter (dbTransformBBox (getSGq fig bBox) (getSGq inst transform))))
    (setq c (SfyCenter (getSGq inst bBox)))
    (setq dx (difference (car p) (car c)))
    (setq dy (difference (cadr p) (cadr c)))
    (setq q
      (if (geqp (abs dx) (abs dy))
        (list (plus (car p) (if (lessp dx 0.0) (minus step) step)) (cadr p))
        (list (car p) (plus (cadr p) (if (lessp dy 0.0) (minus step) step)))))
    (setq w (car (schCreateWire cv "draw" "full" (list p q) (quotient 1.0 dbu) (quotient 1.0 dbu) 0.0)))
    (unless w
      (return (sprintf nil "conn: stub wire for %s.%s failed" (getSGq inst name) term)))
    (unless (and (dbReplaceProp w "schemifyStub" "string" "1")
                 (schCreateWireLabel cv w q net "lowerLeft" "R0" "stick" step nil))
      (return (sprintf nil "conn: label %s failed" net)))
    (return t)))

;; The figure of pin object pin: ~>fig, else the first of ~>figs (virtuoso-bridge-lite
;; reads both: a release may answer only one); nil for nil.
(procedure (SfyPinFig pin)
  (and pin (or (car (errset (getSGq pin fig))) (car (car (errset (getSGq pin figs)))))))

;; Contract with sfy_retarget.il (spec 10.3): draws a stub + label for each (terminal net)
;; of conns; returns the number drawn, or nil when any failed (each failure printed).
(procedure (SfyPlaceConn cv inst conns)
  (let ((n 0) (ok t) (why nil))
    (foreach c conns
      (setq why (SfyStubAt cv inst (car c) (cadr c)))
      (if (stringp why)
        (progn (setq ok nil) (printf "schemify: %s\n" why))
        (setq n (add1 n))))
    (and ok n)))

;; Contract with sfy_retarget.il (spec 10.3): sets each (cdfName value) of sfyParams on
;; the instance CDF in the CDF's own type (numbers as %.12g text for string parameters: SI
;; precision without float noise such as 1.7999999999999999e-07, which a PDK callback that
;; snaps to its grid with floor would drop one step; a string value is read by atoi/atof
;; for int/float ones); with sfyCallbacks, then runs the set parameters' callbacks
;; (SfyCallbacks). Returns t, or a string naming the first failure: a parameter missing, one
;; the PDK locks (editable "nil": map the editable one, such as fingers rather than nf), or
;; "callback: ..." for a callback that failed (the values are set; SfyRtPlace keeps the device).
(procedure (SfySetParams sfyInst sfyParams sfyCallbacks)
  (prog (sfyCdf sfyP sfyV sfyTy sfyWhy sfyNames)
    (setq sfyCdf (cdfGetInstCDF sfyInst))
    (unless sfyCdf (return "the instance has no CDF"))
    (foreach sfyQ (getSGq sfyCdf parameters)
      (setq sfyP (assoc (getSGq sfyQ name) sfyParams))
      (cond
        ((null sfyP) nil)
        ((equal (getSGq sfyQ editable) "nil")
          (unless sfyWhy
            (setq sfyWhy (sprintf nil "parameter %s is read-only in the PDK; map the editable one"
                           (getSGq sfyQ name)))))
        (t
          (setq sfyTy (getSGq sfyQ paramType))
          (setq sfyV (cadr sfyP))
          (setq sfyV
            (cond
              ((and (equal sfyTy "int") (stringp sfyV)) (atoi sfyV))
              ((equal sfyTy "int") (if (fixp sfyV) sfyV (round sfyV)))
              ((and (equal sfyTy "float") (stringp sfyV)) (atof sfyV))
              ((equal sfyTy "float") (float sfyV))
              ((stringp sfyV) sfyV)
              ((fixp sfyV) (sprintf nil "%d" sfyV))
              (t (sprintf nil "%.12g" sfyV))))
          (setSGq sfyQ sfyV value)
          (when (and sfyCallbacks (SfyHasCallback sfyQ))
            (setq sfyNames (cons (getSGq sfyQ name) sfyNames))))))
    (foreach sfyP sfyParams
      (unless (or sfyWhy (cdfFindParamByName sfyCdf (car sfyP)))
        (setq sfyWhy (sprintf nil "no CDF parameter %s" (car sfyP)))))
    (when (and sfyNames (null sfyWhy))
      (foreach sfyN (SfyCallbacks sfyInst (reverse sfyNames))
        (unless sfyWhy
          (setq sfyWhy (sprintf nil "callback: the CDF callback of %s failed; what it derives may be stale (?callbacks nil skips callbacks)"
                         sfyN)))))
    (return (or sfyWhy t))))

;; ("textview" view file text): writes the view's master file.
(procedure (SfyLoadText c lr)
  (let ((tlib (arrayref c "lib")) (cell (arrayref c "name")) (f (cdr (cadr lr)))
        (id nil) (p nil) (text nil))
    (setq text (nth 2 f))
    (cond
      ((and (ddGetObj tlib cell (nth 0 f) (nth 1 f)) (not sfyOverwrite))
        (SfyRefuse (car lr) (sprintf nil "%s/%s/%s exists; pass ?overwrite t" tlib cell (nth 0 f))))
      ((null (setq id (ddGetObj tlib cell (nth 0 f) (nth 1 f) nil "a")))
        (SfyRefuse (car lr) (sprintf nil "cannot create %s/%s/%s/%s" tlib cell (nth 0 f) (nth 1 f))))
      ((null (setq p (outfile (ddGetObjWritePath id) "w")))
        (SfyRefuse (car lr) (sprintf nil "cannot write %s" (ddGetObjWritePath id))))
      (t
        (foreach chunk (if (stringp text) (list text) text)
          (fprintf p "%s" chunk))
        (close p)
        ;; Not ahdlUpdateViewInfo: it may open a port-mismatch dialog that blocks the CIW.
        (SfyLog (sprintf nil "wrote %s/%s/%s; if netlisting reports stale view info, run ahdlUpdateViewInfo(%L ?cell %L ?view %L)"
                  tlib cell (nth 0 f) tlib cell (nth 0 f)))
        (SfyCount "textview")))))
