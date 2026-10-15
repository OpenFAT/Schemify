;; sfy_io.il: the .sfyx writer, reader and log (interop spec sections 4 and 5).
;;
;; Prefix form only (spec 5.1). Dynamic variables bound by the entry points (sfy_cells.il)
;; and read here: sfyLogPort (the .log port), sfyCountTab and sfyCountKeys (record and
;; object counts), sfyFails (refusals so far).

;; ---------------------------------------------------------------- strings

;; The escape of every one-character string that needs one (spec 4.1): \\ \" \n \t \r, and
;; \ooo for the other bytes below 0x20 and 0x7f. Built once into the global sfyEscapes,
;; which loading this file resets (a session that loaded an older schemify.il rebuilds it).
(setq sfyEscapes nil)
(procedure (SfyEscapes)
  (unless (and (boundp 'sfyEscapes) sfyEscapes)
    (setq sfyEscapes (makeTable "sfyEscapes" nil))
    (for i 1 31
      (setarray sfyEscapes (symbolToString (intToChar i))
        (sprintf nil "\\%d%d%d" (quotient i 64) (mod (quotient i 8) 8) (mod i 8))))
    (setarray sfyEscapes (symbolToString (intToChar 127)) "\\177")
    ;; Keyed by character code: some IC builds do not honour \n \t in string literals
    ;; (virtuoso-bridge-lite ramic_bridge.il), and a key read as "n" would escape every n.
    (setarray sfyEscapes (SfyChr 10) "\\n")
    (setarray sfyEscapes (SfyChr 9) "\\t")
    (setarray sfyEscapes (SfyChr 13) "\\r")
    (setarray sfyEscapes "\\" "\\\\")
    (setarray sfyEscapes "\"" "\\\""))
  sfyEscapes)

;; Writes string s quoted and escaped. parseString with "" splits into characters (or
;; bytes: either way every byte >= 0x80 passes through raw). Flushes every 1000 pieces so no
;; single fprintf argument nears SKILL's 8191-character string limits.
(procedure (SfyPutStr port s)
  (let ((tbl (SfyEscapes)) (acc nil) (n 0) e)
    (fprintf port "\"")
    (foreach c (parseString s "")
      (setq e (arrayref tbl c))
      (setq acc (cons (if e e c) acc))
      (setq n (add1 n))
      (when (geqp n 1000)
        (fprintf port "%s" (buildString (reverse acc) ""))
        (setq acc nil)
        (setq n 0)))
    (when acc (fprintf port "%s" (buildString (reverse acc) "")))
    (fprintf port "\"")))

;; Writes one element: a string, an integer (%d of an integer only) or a list of elements
;; (nil is the empty list). Anything else is a bug in the caller and signals.
(procedure (SfyPutValue port v)
  (cond
    ((stringp v) (SfyPutStr port v))
    ((fixp v) (fprintf port "%d" v))
    ((listp v)
      (let ((sep nil))
        (fprintf port "(")
        (foreach x v
          (when sep (fprintf port " "))
          (setq sep t)
          (SfyPutValue port x))
        (fprintf port ")")))
    (t (error "SfyPutValue: %L is not a string, integer or list" v))))

;; t when v can be written: a string, an integer, or a list of such (nil included).
(procedure (SfyWritable v)
  (cond
    ((stringp v) t)
    ((fixp v) t)
    ((listp v) (not (exists x v (not (SfyWritable x)))))
    (t nil)))

;; Writes the record line (tag fields...) and counts it under its tag. Every field is
;; checked first, so a bad one signals before anything is written: a caller's errset then
;; costs that record, never a half line that would make the whole file unreadable.
(procedure (SfyPut port tag fields)
  (unless (SfyWritable fields)
    (error "SfyPut: record %s has a field that is not a string, integer or list: %L" tag fields))
  (fprintf port "(")
  (SfyPutStr port tag)
  (foreach f fields
    (fprintf port " ")
    (SfyPutValue port f))
  (fprintf port ")\n")
  (SfyCount tag))

;; Bytes of the UTF-8 character that piece c (from parseString "") starts; 0 when c is a
;; continuation byte. GUESS(G17): a character-splitting SKILL hands whole characters
;; (strlen > 1); a byte-splitting one hands bytes, whose code may read back signed.
(procedure (SfyCharBytes c)
  (let ((b 0))
    (if (greaterp (strlen c) 1)
      (strlen c)
      (progn
        (setq b (charToInt (stringToSymbol c)))
        (when (lessp b 0) (setq b (plus b 256)))
        (cond
          ((lessp b 128) 1)
          ((lessp b 192) 0)
          ((lessp b 224) 2)
          ((lessp b 240) 3)
          (t 4))))))

;; A text field (spec 4.1) from segs, strings whose concatenation is the text (each under
;; SKILL's 8191-character limit; a file's lines, say): one string when the text is at most
;; 2000 bytes, else a list of chunks of at most 2000 bytes cut on character boundaries,
;; greedily, as interop::sfyx::text does.
(procedure (SfyText segs)
  (let ((total 0) (chunks nil) (cur nil) (n 0) k)
    (foreach s segs (setq total (plus total (strlen s))))
    (if (leqp total 2000)
      (buildString segs "")
      (progn
        (foreach s segs
          (foreach c (parseString s "")
            (setq k (SfyCharBytes c))
            (when (and (greaterp k 0) (greaterp (plus n k) 2000))
              (setq chunks (cons (buildString (reverse cur) "") chunks))
              (setq cur nil)
              (setq n 0))
            (setq cur (cons c cur))
            (setq n (plus n (strlen c)))))
        (when cur (setq chunks (cons (buildString (reverse cur) "") chunks)))
        (reverse chunks)))))

;; The concatenation of a text field (a string or a list of strings).
(procedure (SfyTextValue v)
  (if (stringp v) v (buildString v "")))

;; ---------------------------------------------------------------- atoms and ordering

;; A value as the string the dump writes for it: strings as they are, symbols by name,
;; integers with %d, floats with %.17g (exact round trip, G9), t as "t", nil as "", any
;; other list in print form.
(procedure (SfyAtomStr v)
  (cond
    ((stringp v) v)
    ((null v) "")
    ((equal v t) "t")
    ((symbolp v) (symbolToString v))
    ((fixp v) (sprintf nil "%d" v))
    ((floatp v) (sprintf nil "%.17g" v))
    (t (sprintf nil "%L" v))))

;; v, or "" when it is not a string.
(procedure (SfyStrOr v)
  (if (stringp v) v ""))

;; True for the ways Virtuoso spells yes in CDF flags and boolean properties.
(procedure (SfyYes v)
  (or (equal v t) (and (stringp v) (member v (list "yes" "t" "TRUE" "true")) t)))

;; The dump order (spec 4.2): integers by value, strings by bytes (alphalessp, G12), lists
;; lexicographically with a prefix first; integer < string < list.
(procedure (SfyLess a b)
  (cond
    ((and (fixp a) (fixp b)) (lessp a b))
    ((and (stringp a) (stringp b)) (alphalessp a b))
    ((and (listp a) (listp b))
      (prog ()
        (while (and a b)
          (cond
            ((SfyLess (car a) (car b)) (return t))
            ((SfyLess (car b) (car a)) (return nil)))
          (setq a (cdr a))
          (setq b (cdr b)))
        (return (and (null a) (not (null b))))))
    ((fixp a) t)
    ((stringp a) (listp b))
    (t nil)))

;; entries (key data...) sorted by key.
(procedure (SfySortByKey entries)
  (sort entries (lambda (x y) (SfyLess (car x) (car y)))))

;; ---------------------------------------------------------------- reading

;; Field kinds per record tag (spec 4.2): s string, i integer, l list, x text.
(procedure (SfySchema tag)
  (cadr (assoc tag
    '(("sfyx" "is") ("lib" "s") ("cell" "ss") ("cdf" "x") ("param" "sssssllx")
      ("siminfo" "sl") ("cv" "ssi") ("prop" "isss") ("inst" "issssiis") ("wire" "ili")
      ("label" "iisiisssisss") ("spin" "isssssiis") ("term" "sss") ("pinfig" "isiiii")
      ("shape" "isssli") ("conn" "iss") ("textview" "ssx") ("maestro" "sssss")
      ("var" "sss") ("analysis" "sil") ("corner" "si") ("model" "sss") ("output" "sss")
      ("spec" "sss") ("defn" "sx") ("intent" "isl") ("end" "")))))

;; nil when x is .sfyx data (strings, 32-bit integers, lists of them), else what is wrong.
(procedure (SfyCheckData x depth)
  (cond
    ((greaterp depth 32) "lists nested too deeply")
    ((stringp x) nil)
    ((fixp x)
      (if (and (geqp x (difference -2147483647 1)) (leqp x 2147483647)) nil "integer outside 32 bits"))
    ((listp x)
      (prog (msg)
        (foreach e x
          (setq msg (SfyCheckData e (add1 depth)))
          (when msg (return msg)))
        (return nil)))
    (t (sprintf nil "%L is not a string, integer or list" x))))

;; nil when field v has kind k (a one-character string of SfySchema).
(procedure (SfyKindOk k v)
  (case k
    ("s" (stringp v))
    ("i" (fixp v))
    ("l" (listp v))
    ("x" (or (stringp v) (and (listp v) (not (exists e v (not (stringp e)))))))
    (t nil)))

;; nil when rec is a well-formed record, else what is wrong.
(procedure (SfyCheckRecord rec)
  (let ((schema nil) (msg nil) (fields nil))
    (cond
      ((not (listp rec)) "a record is a parenthesised list")
      ((not (stringp (car rec))) "a record starts with a string tag")
      ((setq msg (SfyCheckData rec 0)) msg)
      ((null (setq schema (SfySchema (car rec)))) (sprintf nil "unknown record %s" (car rec)))
      ((nequal (strlen schema) (length (cdr rec)))
        (sprintf nil "%s takes %d fields, has %d" (car rec) (strlen schema) (length (cdr rec))))
      (t
        (setq fields (cdr rec))
        (prog (k)
          (foreach kind (parseString schema "")
            (setq k (add1 (or k 0)))
            (unless (SfyKindOk kind (car fields))
              (return (sprintf nil "%s: field %d is not %s" (car rec) k
                (case kind ("s" "a string") ("i" "an integer") ("l" "a list") (t "text")))))
            (setq fields (cdr fields)))
          (return nil))))))

;; Reads the .sfyx file at path with lineread, which parses without evaluating (D7).
;; Returns (records nil nil), records a list of (line record), or (nil line message) for
;; the first malformed line, a missing or wrong header or a missing ("end") (a truncated
;; file).
(procedure (SfyReadRecords path)
  (let ((p (infile path)) (line 0) (recs nil) (err nil) (done nil) r form msg)
    (unless p (setq err (list 0 (sprintf nil "cannot read %s" path))))
    (while (and p (not done) (not err))
      (setq line (add1 line))
      (setq r (errset (lineread p)))
      (cond
        ((null r) (setq err (list line "not readable as SKILL data")))
        ((null (car r)) (setq done t))
        ((equal (car r) t) nil)
        (t
          (setq form (car r))
          (cond
            ((cdr form) (setq err (list line "more than one record on the line")))
            ((setq msg (SfyCheckRecord (car form))) (setq err (list line msg)))
            (t (setq recs (cons (list line (car form)) recs)))))))
    (when p (close p))
    (setq recs (reverse recs))
    (cond
      (err (cons nil err))
      ((null recs) (list nil 0 "empty file"))
      ((or (nequal (car (cadr (car recs))) "sfyx") (nequal (cadr (cadr (car recs))) 1))
        (list nil (car (car recs)) "not an .sfyx v1 file (header (\"sfyx\" 1 ...))"))
      ((nequal (car (cadr (car (last recs)))) "end")
        (list nil (sub1 line) "missing (\"end\"): the file is truncated"))
      (t (list recs nil nil)))))

;; ---------------------------------------------------------------- log

;; Opens <path>.log and binds nothing: the entry point let-binds sfyLogPort to the result.
(procedure (SfyLogOpen path)
  (let ((p (outfile (strcat path ".log") "w")))
    (unless p (error "schemify: cannot write %s.log" path))
    p))

;; One log line, flushed at once: a run that hangs on a dialog or is killed still leaves
;; every line before it on disk.
(procedure (SfyLog msg)
  (fprintf sfyLogPort "%s\n" msg)
  (drain sfyLogPort))

;; The message of the error errset last caught, as printed text: errset.errset, the
;; property errset of the symbol errset (getqq). Read it right after the errset that
;; caught the error, and not inside another errset (virtuoso-bridge-lite reads it bare): an
;; errset clears it.
(procedure (SfyErrMsg)
  (sprintf nil "%L" (getqq errset errset)))

;; The one-character string of character code n (built from the code: some IC builds do
;; not honour \n \t in string literals).
(procedure (SfyChr n)
  (symbolToString (intToChar n)))

;; Every Cadence function the scripts call (tests/skill_lint.rs checks it equals the mock's
;; signature table).
(procedure (SfyCadenceFns)
  '(
    ddGetObj ddGetObjWritePath dbOpenCellViewByType dbSave dbClose dbPurge
    dbGetOpenCellViews dbDeleteObject dbFindTermByName dbTransformPoint dbTransformBBox
    dbGetTermNetExpr dbMakeNet dbCreateTerm dbCreatePin dbCreateTermNetExpr dbCreateRect
    dbCreateLine dbCreatePath dbCreatePolygon dbCreateEllipse dbCreateArc dbCreateLabel
    dbReplaceProp dbCreateProp dbProduceMemName schCreateInst schCreateWire
    schCreateWireLabel schCreateNetExpression schCreatePin schCreateNoteLabel schGlueLabel
    schCheck schSolder cdfGetBaseCellCDF cdfCreateBaseCellCDF cdfDeleteCDF cdfCreateParam
    cdfSaveCDF cdfGetInstCDF cdfFindParamByName cdfGetCellCDF cdfUpdateInstParam simulator
    design resultsDir save analysis desVar run getData value maeOpenSetup maeCloseSession
    maeSaveSetup maeGetSetup maeCreateTest maeDeleteTest maeSetAnalysis
    maeGetEnabledAnalysis maeGetAnalysis maeSetVar maeGetVar maeSetCorner maeAddOutput
    maeGetTestOutputs maeSetSpec maeSetEnvOption maeGetEnvOption maeSetSimOption
    maeGetSimOption axlGetTest axlGetMainSetupDB axlGetCorner axlGetEnabled axlGetModels
    axlGetModel axlPutModel axlGetModelFile axlGetModelSection axlSetModelFile
    axlSetModelSection axlGetVars axlGetVar axlGetVarValue axlGetSpecData))

;; Logs the Virtuoso version and every Cadence function this release cannot call (also
;; printed to the CIW), so one run tells what a missing function would cost.
(procedure (SfyPreflight)
  (let ((missing nil))
    (SfyLog (sprintf nil "virtuoso %s" (SfyAtomStr (car (errset (getVersion t))))))
    (foreach f (SfyCadenceFns)
      (unless (car (errset (isCallable f)))
        (setq missing (cons f missing))))
    (setq missing (reverse missing))
    (if missing
      (progn
        (SfyLog (sprintf nil "preflight: not callable here: %L" missing))
        (printf "schemify: not callable in this Virtuoso: %L\n" missing))
      (SfyLog "preflight: every function callable"))
    missing))

;; Logs a refusal (an object or a cellview that did not land, or a malformed file) with
;; its .sfyx line (0: none) and counts it toward FAIL.
(procedure (SfyRefuse line msg)
  (setq sfyFails (add1 sfyFails))
  (if (greaterp line 0)
    (SfyLog (sprintf nil "refused line %d: %s" line msg))
    (SfyLog (sprintf nil "refused: %s" msg))))

;; Counts one key (a record tag or an object kind).
(procedure (SfyCount key)
  (let ((n (arrayref sfyCountTab key)))
    (unless n
      (setq sfyCountKeys (cons key sfyCountKeys))
      (setq n 0))
    (setarray sfyCountTab key (add1 n))))

;; Writes the counts (sorted, one "count <key> <n>" line each) and PASS or FAIL, closes the
;; log, and returns t on PASS.
(procedure (SfyLogClose)
  (foreach k (sort (copy sfyCountKeys) nil)
    (SfyLog (sprintf nil "count %s %d" k (arrayref sfyCountTab k))))
  (SfyLog (if (zerop sfyFails) "PASS" "FAIL"))
  (close sfyLogPort)
  (zerop sfyFails))
