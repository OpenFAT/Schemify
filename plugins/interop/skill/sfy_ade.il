;; sfy_ade.il: maestro (ADE Explorer/Assembler) setups to and from .sfyx (interop spec
;; sections 4.2, 5.2, 9). Entry points called by sfy_cells.il: SfyAdeExport (during
;; SfyExport) and SfyAdeImport (during SfyImport).
;;
;; Every decision about meaning is made in Rust (virtuoso::ade); this file only copies the
;; maestro setup to records and back. Prefix form only (spec 5.1). Reads the dynamic
;; variables of sfy_cells.il: the log state, sfyOverwrite and sfyTargets.
;; GUESS(G6): maeOpenSetup ?mode "r" reads a setup in a background session, no window.

;; ================================================================ export

;; Writes the maestro records (spec 9) of every cell of cells (names in library lib, sorted)
;; that has a maestro view: per test (by name) maestro, global vars, enabled analyses,
;; test model files, corners with their models and vars, outputs, specs, definition files.
(procedure (SfyAdeExport port lib cells)
  (foreach cell cells
    (when (ddGetObj lib cell "maestro")
      (SfyAdeDumpCell port lib cell))))

;; One maestro view, opened read-only and always closed.
(procedure (SfyAdeDumpCell port lib cell)
  (let ((sess (maeOpenSetup lib cell "maestro" ?mode "r")) (sdb nil))
    (if (null sess)
      (SfyRefuse 0 (sprintf nil "cannot open %s/%s/maestro" lib cell))
      (unwindProtect
        (progn
          (setq sdb (axlGetMainSetupDB sess))
          (if (null sdb)
            (SfyRefuse 0 (sprintf nil "%s/%s/maestro: no setup database" lib cell))
            (foreach test (sort (copy (maeGetSetup ?session sess)) nil)
              (SfyAdeDumpTest port lib cell sess sdb test))))
        (maeCloseSession ?session sess)))))

;; The records of one test, in spec 9 order.
(procedure (SfyAdeDumpTest port lib cell sess sdb test)
  (let ((outs nil) (specs nil) (defs nil) (c nil) (m nil) (vl nil) (o nil) (p nil)
        (line nil) (segs nil) (e nil))
    (SfyPut port "maestro" (list test lib cell "schematic" "spectre"))
    ;; GUESS(G18): maeGetSetup ?typeName "variables" lists the global variable names.
    (foreach n (sort (copy (maeGetSetup ?typeName "variables" ?session sess)) nil)
      (SfyPut port "var" (list n (SfyAtomStr (maeGetVar n ?session sess)) "")))
    (foreach a (sort (copy (maeGetEnabledAnalysis test ?session sess)) nil)
      (SfyPut port "analysis"
        (list a 1 (mapcar (lambda (kv) (list (SfyAtomStr (car kv)) (SfyAtomStr (cadr kv))))
                    (maeGetAnalysis test a ?session sess)))))
    (foreach mf (maeGetEnvOption test ?option "modelFiles" ?session sess)
      (SfyPut port "model" (list "" (SfyAtomStr (car mf)) (SfyAtomStr (cadr mf)))))
    (foreach cn (sort (copy (maeGetSetup ?typeName "corners" ?session sess)) nil)
      (setq c (axlGetCorner sdb cn))
      (if (null c)
        (SfyRefuse 0 (sprintf nil "maestro %s: corner %s has no handle" test cn))
        (progn
          (SfyPut port "corner" (list cn (if (axlGetEnabled c) 1 0)))
          ;; GUESS(G19): axlGetModels gives the corner's model names in setup order.
          (foreach mn (axlGetModels c)
            (setq m (axlGetModel c mn))
            (if (or (null m) (equal m 0))
              (SfyRefuse 0 (sprintf nil "maestro %s: corner %s model %s has no handle" test cn mn))
              (SfyPut port "model"
                (list cn (SfyStrOr (axlGetModelFile m)) (SfyStrOr (axlGetModelSection m))))))
          ;; axlGetVars: (handle names) (IC23.1 reference).
          (setq vl (axlGetVars c))
          (foreach vn (sort (copy (cadr vl)) nil)
            (SfyPut port "var"
              (list vn (SfyAtomStr (axlGetVarValue (axlGetVar c vn))) cn))))))
    (foreach so (maeGetTestOutputs test ?session sess)
      (setq e (getSGq so expr))
      (setq o (getSGq so signalName))
      (cond
        ((stringp e) (setq outs (cons (list (getSGq so name) "point" e) outs)))
        ((stringp o) (setq outs (cons (list (getSGq so name) "net" o) outs)))
        (t (SfyLog (sprintf nil "skipped: maestro %s output %L without an expression" test
                     (getSGq so name))))))
    (foreach x (SfySortByKey outs)
      (SfyPut port "output" x)
      ;; GUESS(G21): axlGetSpecData gives (op value) pairs, op "gt" "lt" or "range" with
      ;; value (lo hi).
      (foreach sp (axlGetSpecData sdb (car x) test)
        (case (SfyAtomStr (car sp))
          ("gt" (setq specs (cons (list (list (car x) "gt") (SfyAtomStr (cadr sp))) specs)))
          ("lt" (setq specs (cons (list (list (car x) "lt") (SfyAtomStr (cadr sp))) specs)))
          ("range"
            (setq specs (cons (list (list (car x) "gt") (SfyAtomStr (car (cadr sp)))) specs))
            (setq specs (cons (list (list (car x) "lt") (SfyAtomStr (cadr (cadr sp)))) specs)))
          (t (SfyLog (sprintf nil "skipped: maestro %s spec %s %L" test (car x) (car sp)))))))
    (foreach x (SfySortByKey specs)
      (SfyPut port "spec" (list (car (car x)) (cadr (car x)) (cadr x))))
    ;; GUESS(G22): the definitionFiles environment option is a list of file paths.
    (foreach f (maeGetEnvOption test ?option "definitionFiles" ?session sess)
      (setq p (infile f))
      (if (null p)
        (SfyRefuse 0 (sprintf nil "maestro %s: cannot read definition file %s" test f))
        (progn
          (setq segs nil)
          (while (gets line p) (setq segs (cons line segs)))
          (close p)
          (setq defs (cons (list (car (last (parseString f "/"))) (SfyText (reverse segs)))
                       defs)))))
    (foreach x (SfySortByKey defs)
      (SfyPut port "defn" x))))

;; ================================================================ import

;; Creates the maestro setups that recs, the (line record) pairs of every maestro test of
;; the file, describe. A test lands in the target library of its cell (sfyTargets, else
;; lib, else the record's library); an existing maestro view is refused unless sfyOverwrite,
;; which replaces the test of the same name. Each refused call is logged with its line.
(procedure (SfyAdeImport recs lib)
  (let ((tests nil) (cur nil))
    (foreach lr recs
      (if (equal (car (cadr lr)) "maestro")
        (progn
          (when cur (setq tests (cons (reverse cur) tests)))
          (setq cur (list lr)))
        (setq cur (cons lr cur))))
    (when cur (setq tests (cons (reverse cur) tests)))
    (foreach test (reverse tests)
      (SfyAdeLoadTest test lib))))

;; One test: open (creating) the maestro view in append mode, fill it, save, close.
(procedure (SfyAdeLoadTest test lib)
  (let ((line (car (car test))) (m (cadr (car test))) (tlib nil) (cell nil) (sess nil))
    (setq cell (nth 3 m))
    (setq tlib (or (arrayref sfyTargets (strcat (nth 2 m) "/" cell)) lib (nth 2 m)))
    (cond
      ((null (ddGetObj tlib cell))
        (SfyRefuse line (sprintf nil "maestro %s: no cell %s/%s" (nth 1 m) tlib cell)))
      ((and (ddGetObj tlib cell "maestro") (not sfyOverwrite))
        (SfyRefuse line (sprintf nil "%s/%s/maestro exists; pass ?overwrite t" tlib cell)))
      ((null (setq sess (maeOpenSetup tlib cell "maestro" ?mode "a")))
        (SfyRefuse line (sprintf nil "cannot open %s/%s/maestro" tlib cell)))
      (t
        (unwindProtect
          (SfyAdeFill test tlib cell sess)
          (maeCloseSession ?session sess))))))

;; t when opts is a list of (key value) strings.
(procedure (SfyAdeOptsOk opts)
  (not (exists kv opts
         (not (and (listp kv) (equal (length kv) 2) (stringp (car kv)) (stringp (cadr kv)))))))

;; A model name for file in corner cn not yet in used (a list of "corner/name"): the file's
;; base name, else base_k.
(procedure (SfyAdeModelName cn file used)
  (let ((base (car (last (parseString file "/")))) (k 1) (n nil))
    (setq n base)
    (while (member (strcat cn "/" n) used)
      (setq k (add1 k))
      (setq n (sprintf nil "%s_%d" base k)))
    n))

;; Writes the records of one test into the open session sess.
(procedure (SfyAdeFill test tlib cell sess)
  (let ((m (cadr (car test))) (name nil) (sdb nil) (models nil) (defs nil) (specs nil)
        (used nil) (opts nil) (f nil) (line nil) (c nil) (mh nil) (mn nil) (id nil) (p nil)
        (ok nil) (gt nil) (lt nil) (outs nil))
    (setq name (nth 1 m))
    (prog ()
      (when (member name (maeGetSetup ?session sess))
        (unless (maeDeleteTest name ?session sess)
          (SfyRefuse (car (car test)) (sprintf nil "cannot replace maestro test %s" name))
          (return nil)))
      (unless (maeCreateTest name ?lib tlib ?cell cell ?view (nth 4 m) ?simulator (nth 5 m)
                ?session sess)
        (SfyRefuse (car (car test)) (sprintf nil "cannot create maestro test %s" name))
        (return nil))
      (SfyCount "maestro")
      (setq sdb (axlGetMainSetupDB sess))
      (foreach lr (cdr test)
        (setq line (car lr))
        (setq f (cdr (cadr lr)))
        (setq ok
          (case (car (cadr lr))
            ("var"
              (if (equal (nth 2 f) "")
                (maeSetVar (nth 0 f) (nth 1 f) ?session sess)
                (maeSetVar (nth 0 f) (nth 1 f) ?typeName "corner" ?typeValue (list (nth 2 f))
                  ?session sess)))
            ("analysis"
              (if (SfyAdeOptsOk (nth 2 f))
                (maeSetAnalysis name (nth 0 f) ?enable (equal (nth 1 f) 1) ?options (nth 2 f)
                  ?session sess)
                nil))
            ("corner" (maeSetCorner (nth 0 f) ?enabled (equal (nth 1 f) 1) ?session sess))
            ("model"
              (if (equal (nth 0 f) "")
                (setq models (cons (list (nth 1 f) (nth 2 f)) models))
                (progn
                  (setq c (and sdb (axlGetCorner sdb (nth 0 f))))
                  (setq mn (SfyAdeModelName (nth 0 f) (nth 1 f) used))
                  (setq used (cons (strcat (nth 0 f) "/" mn) used))
                  (setq mh (and c (axlPutModel c mn)))
                  (and mh (axlSetModelFile mh (nth 1 f))
                    (or (equal (nth 2 f) "") (axlSetModelSection mh (nth 2 f)))))))
            ("output"
              (setq outs (cons (nth 0 f) outs))
              (if (equal (nth 1 f) "net")
                (maeAddOutput (nth 0 f) name ?outputType "net" ?signalName (nth 2 f) ?session sess)
                (maeAddOutput (nth 0 f) name ?outputType "point" ?expr (nth 2 f) ?session sess)))
            ("spec" (setq specs (cons (list line f) specs)))
            ("defn"
              (setq id (ddGetObj tlib cell "maestro" (nth 0 f) nil "a"))
              (setq p (and id (outfile (ddGetObjWritePath id) "w")))
              (when p
                (foreach chunk (if (stringp (nth 1 f)) (list (nth 1 f)) (nth 1 f))
                  (fprintf p "%s" chunk))
                (close p)
                (setq defs (cons (ddGetObjWritePath id) defs))))
            (t nil)))
        (if ok
          (SfyCount (strcat "maestro " (car (cadr lr))))
          (SfyRefuse line (sprintf nil "maestro %s: %s %L did not land" name (car (cadr lr)) f))))
      ;; One spec per output (GUESS(G21): both bounds are a ?range (lo hi)).
      (foreach o (reverse outs)
        (setq gt nil)
        (setq lt nil)
        (foreach s specs
          (when (equal (car (cadr s)) o)
            (setq line (car s))
            (if (equal (cadr (cadr s)) "gt")
              (setq gt (caddr (cadr s)))
              (setq lt (caddr (cadr s))))))
        (when (or gt lt)
          (setq ok
            (cond
              ((and gt lt) (maeSetSpec o name ?range (list gt lt) ?session sess))
              (gt (maeSetSpec o name ?gt gt ?session sess))
              (t (maeSetSpec o name ?lt lt ?session sess))))
          (if ok
            (SfyCount "maestro spec set")
            (SfyRefuse line (sprintf nil "maestro %s: spec of %s did not land" name o)))))
      (when models (setq opts (cons (list "modelFiles" (reverse models)) opts)))
      (when defs (setq opts (cons (list "definitionFiles" (reverse defs)) opts)))
      (when (and opts (null (maeSetEnvOption name ?options (reverse opts) ?session sess)))
        (SfyRefuse (car (car test)) (sprintf nil "maestro %s: model or definition files did not land" name)))
      (unless (maeSaveSetup ?lib tlib ?cell cell ?view "maestro" ?session sess)
        (SfyRefuse (car (car test)) (sprintf nil "cannot save %s/%s/maestro" tlib cell))))))
