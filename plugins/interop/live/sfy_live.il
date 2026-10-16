;; sfy_live.il: the Virtuoso side of the live test kit (plugins/interop/live/README.md).
;; Loaded after schemify.il by the generated session files (session1.il, session2.il), which
;; call these procedures with concrete paths. Every check writes one line to the results
;; file, flushed at once, so a session that dies on a dialog still leaves every check before
;; it:
;;   PASS <id> <detail>      FAIL <id> <detail>      INFO <id> <detail>
;; Prefix form only (the reader of tests/skill parses this file in CI), every Cadence call
;; inside errset, so one failing step never stops the session.

;; Opens the results file. Call first.
(procedure (SfyLiveBegin path)
  (setq sfyLivePort (outfile path "w"))
  (SfyLiveLine "INFO" "session" (sprintf nil "virtuoso %L" (car (errset (getVersion t) nil))))
  t)

;; Closes the results file. Call last, before (exit). Purges every cellview still open, so
;; exit has nothing to offer for saving (its save form blocks a -nograph session forever).
(procedure (SfyLiveEnd)
  (foreach cv (or (car (errset (dbGetOpenCellViews))) nil)
    (errset (dbPurge cv)))
  (SfyLiveLine "INFO" "session" "end")
  (when sfyLivePort (close sfyLivePort))
  (setq sfyLivePort nil)
  t)

;; One result line, to the results file and the CIW.
(procedure (SfyLiveLine kind id detail)
  (printf "sfylive: %s %s %s\n" kind id detail)
  (when sfyLivePort
    (fprintf sfyLivePort "%s %s %s\n" kind id detail)
    (drain sfyLivePort))
  t)

;; PASS or FAIL by ok.
(procedure (SfyLiveCheck id ok detail)
  (SfyLiveLine (if ok "PASS" "FAIL") id detail)
  ok)

;; The message of the error the last errset caught.
(procedure (SfyLiveErr)
  (let ((e (getqq errset errset)))
    (if e (sprintf nil "SKILL error %L" e) "SKILL error")))

;; Every entry point of schemify.il is callable, and the functions this release lacks.
(procedure (SfyLiveEntryPoints)
  (let ((missing nil) (absent nil))
    (foreach f '(SfyExport SfyExportLib SfyImport SfySelfTest SfyAdeExport
                 SfyAdeImport SfyProbe SfyCharacterize)
      (unless (car (errset (isCallable f))) (setq missing (cons f missing))))
    (SfyLiveCheck "env.entry-points" (null missing)
      (if missing (sprintf nil "not callable: %L" missing) "all callable"))
    (setq absent (car (errset (setof f (SfyCadenceFns) (not (car (errset (isCallable f))))) t)))
    (SfyLiveCheck "env.cadence-functions" (null absent)
      (if absent (sprintf nil "this release lacks %L" absent) "every function the bridge calls exists"))
    (and (null missing) (null absent))))

;; Makes library name at path unless it exists.
(procedure (SfyLiveLib name path)
  (let ((r nil))
    (cond
      ((ddGetObj name) (SfyLiveLine "INFO" (strcat "lib." name) "exists"))
      (t
        (setq r (errset (ddCreateLib name path) t))
        (SfyLiveCheck (strcat "lib." name) (and r (car r))
          (if (and r (car r)) (strcat "created at " path) (SfyLiveErr)))))
    (ddGetObj name)))

;; Copies text file from to file to (logs, /tmp outputs).
(procedure (SfyLiveCopy from to)
  (let ((in (infile from)) (out nil) (line nil))
    (when in
      (setq out (outfile to "w"))
      (while (gets line in)
        (fprintf out "%s" line))
      (close in)
      (close out))
    (and in t)))

;; t when file's text contains needle.
(procedure (SfyLiveGrep file needle)
  (let ((in (infile file)) (line nil) (found nil))
    (when in
      (while (and (not found) (gets line in))
        (when (SfyLiveHas line needle) (setq found t)))
      (close in))
    found))

;; t when string s contains needle (SKILL's index looks for one character only).
(procedure (SfyLiveHas s needle)
  (let ((n (strlen needle)) (k 1) (found nil))
    (while (and (not found) (leqp (plus k n -1) (strlen s)))
      (when (equal (substring s k n) needle) (setq found t))
      (setq k (add1 k)))
    found))

;; SfyImport of file; PASS when it returns t (want t) or nil (want nil).
(procedure (SfyLiveImport id file want lib overwrite callbacks check)
  (let ((r nil) (ok nil))
    (setq r (errset (SfyImport file ?lib lib ?overwrite overwrite ?callbacks callbacks ?check check) t))
    (setq ok (if r (if want (car r) (null (car r))) nil))
    (SfyLiveCheck id ok
      (cond
        ((null r) (SfyLiveErr))
        (want (if (car r) "loaded, log PASS" (sprintf nil "refused: see %s.log" file)))
        (t (if (car r) "loaded although it must be refused" "refused as it must be"))))
    ok))

;; SfyExport of cells (nil = every cell) of lib to file.
(procedure (SfyLiveExport id file lib cells)
  (let ((r (errset (SfyExport file lib cells) t)))
    (SfyLiveCheck id (and r (car r))
      (cond ((null r) (SfyLiveErr)) ((car r) (strcat "dumped to " file))
            (t (sprintf nil "FAIL: see %s.log" file))))))

;; SfyExportLib of a PDK library to file.
(procedure (SfyLiveExportLib id file lib)
  (let ((r (errset (SfyExportLib file lib) t)))
    (SfyLiveCheck id (and r (car r))
      (cond ((null r) (SfyLiveErr)) ((car r) (strcat "dumped to " file))
            (t (sprintf nil "FAIL: see %s.log" file))))))

;; SfySelfTest of lib/cell through scratch; copies its report and dumps into dir.
(procedure (SfyLiveSelfTest id lib cell scratch dir)
  (let ((r (errset (SfySelfTest lib cell scratch) t)))
    (SfyLiveCopy "/tmp/sfy_selftest.txt" (strcat dir "/" id ".txt"))
    (SfyLiveCopy "/tmp/sfy_selftest_a.sfyx" (strcat dir "/" id ".a.sfyx"))
    (SfyLiveCopy "/tmp/sfy_selftest_a.sfyx.log" (strcat dir "/" id ".a.sfyx.log"))
    (SfyLiveCopy "/tmp/sfy_selftest_b.sfyx" (strcat dir "/" id ".b.sfyx"))
    (SfyLiveCopy "/tmp/sfy_selftest_b.sfyx.log" (strcat dir "/" id ".b.sfyx.log"))
    (SfyLiveCheck id (and r (car r))
      (cond ((null r) (SfyLiveErr)) ((car r) "dump, load, dump agree")
            (t (sprintf nil "see %s/%s.txt" dir id))))))

;; SfyProbe into scratch (pdkLib/pdkCell may be nil); copies /tmp/sfy_probe.txt to out.
(procedure (SfyLiveProbe scratch pdkLib pdkCell out)
  (let ((r (errset (if pdkLib (SfyProbe scratch ?pdkLib pdkLib ?pdkCell pdkCell) (SfyProbe scratch)) t)))
    (SfyLiveCopy "/tmp/sfy_probe.txt" out)
    (SfyLiveCheck "probe" (and r t)
      (if r (strcat "written to " out) (SfyLiveErr)))))

;; A cell that must not exist (nothing created by a refused load).
(procedure (SfyLiveAbsent id lib cell)
  (SfyLiveCheck id (null (ddGetObj lib cell))
    (if (ddGetObj lib cell) (sprintf nil "%s/%s exists" lib cell) "nothing created")))

;; A view that must exist (a maestro view written by SfyImport).
(procedure (SfyLivePresent id lib cell view)
  (SfyLiveCheck id (and (ddGetObj lib cell view) t)
    (if (ddGetObj lib cell view) (sprintf nil "%s/%s/%s exists" lib cell view)
      (sprintf nil "%s/%s/%s missing" lib cell view))))

;; With lib/cell/schematic open for editing, SfyImport must refuse file and name the reason.
(procedure (SfyLiveOpenRefusal id file lib cell)
  (let ((cv nil) (r nil) (ok nil))
    (setq cv (car (errset (dbOpenCellViewByType lib cell "schematic" "schematic" "a") t)))
    (cond
      ((null cv)
        (SfyLiveCheck id nil (sprintf nil "cannot open %s/%s/schematic for editing" lib cell)))
      (t
        (setq r (errset (SfyImport file ?overwrite t) t))
        (setq ok (and r (null (car r)) (SfyLiveGrep (strcat file ".log") "open for editing")))
        (errset (dbClose cv) t)
        (SfyLiveCheck id ok
          (if ok "refused while open for editing"
            (sprintf nil "not refused with the reason: see %s.log" file)))))))

;; The Virtuoso-side edit of the teamwork check: sets CDF parameter param of instance inst in
;; lib/cell/schematic to value (a string), checks and saves.
(procedure (SfyLiveEditParam id lib cell inst param value)
  (let ((cv nil) (i nil) (p nil) (ok nil))
    (setq cv (car (errset (dbOpenCellViewByType lib cell "schematic" "schematic" "a") t)))
    (when cv
      (setq i (car (setof x (getSGq cv instances) (equal (getSGq x name) inst))))
      (setq p (and i (cdfFindParamByName (cdfGetInstCDF i) param)))
      (when p
        (setq ok (and (errset (setSGq p value value) t)
                      (errset (schCheck cv) t)
                      (errset (dbSave cv) t)
                      t)))
      (errset (dbClose cv) t))
    (SfyLiveCheck id ok
      (cond ((null cv) (sprintf nil "cannot open %s/%s/schematic" lib cell))
            ((null i) (sprintf nil "no instance %s" inst))
            ((null p) (sprintf nil "%s has no CDF parameter %s" inst param))
            (ok (sprintf nil "%s %s = %s saved" inst param value))
            (t (SfyLiveErr))))))

;; SfyCharacterize of lib/cell into out (spec interop.md 8.3), as one check.
(procedure (SfyLiveCharacterize lib named scratch out users)
  (let ((cells named) (r nil))
    (foreach c (SfyLiveUsedCells lib users)
      (unless (member c cells) (setq cells (append cells (list c)))))
    (if (null cells)
      (SfyLiveCheck "pdk.characterize" t "no PDK cell named or used: nothing characterized")
      (progn
        (setq r (errset (SfyCharacterize out lib cells scratch) t))
        (SfyLiveCheck "pdk.characterize" (and r (car r))
          (cond ((null r) (SfyLiveErr))
                ((car r) (sprintf nil "%d cell(s) written to %s (it names the PDK's parameters: it stays on this machine)" (length cells) out))
                (t (sprintf nil "see %s.log" out))))))))

;; The cells of PDK library lib the user's cells instantiate in their schematics, in
;; first-use order; users is a list of (lib cell), cell "*" for every cell of lib.
(procedure (SfyLiveUsedCells lib users)
  (let ((out nil) (cv nil) (names nil) (o nil))
    (foreach u users
      (setq names (list (cadr u)))
      (when (equal (cadr u) "*")
        (setq o (ddGetObj (car u)))
        (setq names (and o (mapcar (lambda (c) (getSGq c name)) (getSGq o cells)))))
      (foreach n names
        (setq cv (car (errset (dbOpenCellViewByType (car u) n "schematic" nil "r") nil)))
        (when cv
          (foreach i (getSGq cv instances)
            (when (and (equal (getSGq i libName) lib) (not (member (getSGq i cellName) out)))
              (setq out (cons (getSGq i cellName) out)))))))
    (reverse out)))

;; SfyCharacterize of lib/cell at the given points only (the holdout of its rules, spec
;; interop.md 8.3) into out, as one check.
(procedure (SfyLiveCharacterizeCheck lib cell scratch out points)
  (let ((r (errset (SfyCharacterize out lib (list cell) scratch ?points points) t)))
    (SfyLiveCheck (sprintf nil "pdk.holdout.%s.%s" lib cell) (and r (car r))
      (cond ((null r) (SfyLiveErr))
            ((car r) (sprintf nil "%d point(s) measured into %s" (length points) out))
            (t (sprintf nil "see %s.log" out))))))
