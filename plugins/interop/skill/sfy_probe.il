;; sfy_probe.il: the first-contact probe (interop spec 5.5, research virtuoso-skill-db 9).
;; Not part of schemify.il. In a real Virtuoso:
;;   (load "/abs/path/schemify.il") (load "/abs/path/sfy_probe.il") (SfyProbe "scratchLib")
;; then send /tmp/sfy_probe.txt. It touches only scratchLib/sfy_probe: the schematic, opened
;; "w" and purged, never saved, an empty saved schematic sfy_probe_tb (the maestro test's
;; design), and a maestro view it creates and saves (to read it back).
;; With ?pdkLib and ?pdkCell (a PDK transistor) it also places that device on the scratch
;; schematic and records its CDF and callbacks (the lines name PDK parameters). Each section
;; is trapped on its own; a failing section prints "<section> ERROR <message>" and the probe
;; goes on; the file is flushed after every section. Every line names what it settles.

(procedure (SfyProbe sfyScratchLib @key (pdkLib nil) (pdkCell nil))
  (let ((sfyP (outfile "/tmp/sfy_probe.txt" "w")) (sfyCv nil) (sfySym nil) (sfyWs nil) (sfyX nil) (sfyM nil)
        (sfyInst nil) (sfyCdf nil) (sfyPar nil) (sfyS nil) (sfyQ nil) (sfyR nil) (sfySess nil) (sfySdb nil))
    (unless sfyP (error "SfyProbe: cannot write /tmp/sfy_probe.txt"))
    (fprintf sfyP "sfy_probe 2 scratch %s\n" sfyScratchLib)
    (unless (ddGetObj sfyScratchLib)
      (fprintf sfyP "scratch ERROR library %s is not in cds.lib\n" sfyScratchLib))

    ;; Preflight: the release and every Cadence function the scripts call it cannot.
    (unless (errset
              (progn
                (fprintf sfyP "version %L\n" (car (errset (getVersion t))))
                (when (fboundp 'SfyCadenceFns)
                  (fprintf sfyP "not callable %L\n"
                    (setof sfyF (SfyCadenceFns) (not (car (errset (isCallable sfyF))))))))
              t)
      (fprintf sfyP "preflight ERROR\n"))
    ;; The shape of errset.errset (what SfyErrMsg logs).
    (unless (errset
              (progn
                (errset (car 1))
                (fprintf sfyP "errmsg bare %L\n" (getqq errset errset))
                (errset (car 1))
                (fprintf sfyP "errmsg in-errset %L\n" (car (errset (getqq errset errset))))
                (when (fboundp 'SfyErrMsg)
                  (errset (car 1))
                  (fprintf sfyP "errmsg SfyErrMsg %L\n" (SfyErrMsg))))
              t)
      (fprintf sfyP "errmsg ERROR\n"))
    (drain sfyP)

    ;; G9: printing and rounding.
    (unless (errset
              (progn
                (fprintf sfyP "print L-string %L\n" "a\"b\\c\nd\001e")
                (fprintf sfyP "print float L=%L g17=%s\n" (plus 0.1 0.2) (sprintf nil "%.17g" (plus 0.1 0.2)))
                (fprintf sfyP "print round %d %d %d\n" (round 2.5) (round -2.5) (round (times 0.0625 160.0))))
              t)
      (fprintf sfyP "print ERROR\n"))

    ;; G1: orientations, the image of (1 2).
    (unless (errset
              (foreach sfyO (list "R0" "R90" "R180" "R270" "MY" "MYR90" "MX" "MXR90")
                (fprintf sfyP "orient %s %L\n" sfyO (dbTransformPoint (list 1.0 2.0) (list (list 0.0 0.0) sfyO))))
              t)
      (fprintf sfyP "orient ERROR\n"))

    ;; Name expansion (an oracle for the Rust name parser).
    (unless (errset (fprintf sfyP "names %L\n" (dbProduceMemName "A<2:0>,<*2>(b,c),d<0:4:2*2>,vdd!")) t)
      (fprintf sfyP "names ERROR\n"))

    ;; G12 and the text chunker: characters or bytes, signed or unsigned, byte order.
    (unless (errset
              (progn
                (fprintf sfyP "string parseString-utf8 %d\n" (length (parseString "\303\251x" "")))
                (fprintf sfyP "string charToInt-high %d\n"
                  (charToInt (stringToSymbol (car (parseString "\303\251" "")))))
                (fprintf sfyP "string alphalessp B<a %L z<e-acute %L\n"
                  (alphalessp "B" "a") (alphalessp "z" "\303\251")))
              t)
      (fprintf sfyP "string ERROR\n"))

    ;; G9: the escaper round trip over every byte 1..127 and a UTF-8 character.
    (unless (errset
              (when (fboundp 'SfyPutStr)
                (setq sfyX nil)
                (for sfyI 1 127 (setq sfyX (cons (symbolToString (intToChar sfyI)) sfyX)))
                (setq sfyS (strcat (buildString (reverse sfyX) "") "\303\251"))
                (setq sfyQ (outfile "/tmp/sfy_probe_esc.sfyx" "w"))
                (fprintf sfyQ "(")
                (SfyPutStr sfyQ sfyS)
                (fprintf sfyQ ")\n")
                (close sfyQ)
                (setq sfyQ (infile "/tmp/sfy_probe_esc.sfyx"))
                (setq sfyR (lineread sfyQ))
                (close sfyQ)
                (fprintf sfyP "escaper %s\n" (if (equal (car (car sfyR)) sfyS) "PASS" "FAIL")))
              t)
      (fprintf sfyP "escaper ERROR\n"))

    ;; G3: analogLib global symbols and basic pin masters.
    (unless (errset
              (progn
                (foreach sfyC (list "gnd" "vdd")
                  (setq sfySym (dbOpenCellViewByType "analogLib" sfyC "symbol" nil "r"))
                  (fprintf sfyP "global %s terminals %L\n" sfyC (and sfySym (getSGq (getSGq sfySym terminals) name)))
                  (when sfySym (dbClose sfySym)))
                (foreach sfyC (list "ipin" "opin" "iopin" "noConn")
                  (fprintf sfyP "basic %s views %L\n" sfyC
                    (getSGq (getSGq (ddGetObj "basic" sfyC) views) name))))
              t)
      (fprintf sfyP "global ERROR\n"))

    ;; G2 and a reference symbol's anatomy.
    (unless (errset
              (progn
                (setq sfySym (dbOpenCellViewByType "analogLib" "nmos4" "symbol" nil "r"))
                (when sfySym
                  (fprintf sfyP "symbol units=%L dbu=%L type=%L\n" (getSGq sfySym userUnits)
                    (getSGq sfySym DBUPerUU) (getSGq sfySym cellViewType))
                  (foreach sfyF (getSGq sfySym shapes)
                    (fprintf sfyP "symshape %s %L %L %L pin=%L\n" (getSGq sfyF objType) (getSGq sfyF lpp)
                      (if (equal (getSGq sfyF objType) "label") (getSGq sfyF theLabel) (getSGq sfyF bBox))
                      (getSGq sfyF labelType) (and (getSGq sfyF pin) (getSGq (getSGq sfyF pin) name))))
                  (foreach sfyT0 (getSGq sfySym terminals)
                    (fprintf sfyP "symterm %s %s netexpr=%L pins=%L\n" (getSGq sfyT0 name) (getSGq sfyT0 direction)
                      (dbGetTermNetExpr sfyT0) (getSGq (getSGq (getSGq sfyT0 pins) fig) bBox)))
                  (dbClose sfySym)))
              t)
      (fprintf sfyP "symbol ERROR\n"))

    ;; CDF attribute spellings (flags yes/t, display, defValue types) and simInfo.
    (unless (errset
              (progn
                (setq sfyCdf (cdfGetBaseCellCDF (ddGetObj "analogLib" "res")))
                (when sfyCdf
                  (fprintf sfyP "cdf type=%L paramLabelSet=%L\n" (getSGq sfyCdf type) (getSGq sfyCdf paramLabelSet))
                  (foreach sfyQ0 (getSGq sfyCdf parameters)
                    (fprintf sfyP "cdfparam %L %L def=%L num=%L cel=%L store=%L display=%L dontSave=%L\n"
                      (getSGq sfyQ0 name) (getSGq sfyQ0 paramType) (getSGq sfyQ0 defValue) (getSGq sfyQ0 parseAsNumber)
                      (getSGq sfyQ0 parseAsCEL) (getSGq sfyQ0 storeDefault) (getSGq sfyQ0 display) (getSGq sfyQ0 dontSave)))
                  (fprintf sfyP "cdf simInfo %L\n" (getSGq sfyCdf simInfo))))
              t)
      (fprintf sfyP "cdf ERROR\n"))

    ;; The scratch schematic: G2, G4, G5, G10, G11, G13, G14, G15 and boolean properties.
    (setq sfyCv (dbOpenCellViewByType sfyScratchLib "sfy_probe" "schematic" "schematic" "w"))
    (if (null sfyCv)
      (fprintf sfyP "scratch ERROR cannot open %s/sfy_probe/schematic for writing\n" sfyScratchLib)
      (unwindProtect
        (progn
          (unless (errset
                    (progn
                      (fprintf sfyP "sch units=%L dbu=%L\n" (getSGq sfyCv userUnits) (getSGq sfyCv DBUPerUU))
                      (setq sfyWs (schCreateWire sfyCv "draw" "full" (list (list 0.0 0.0) (list 1.0 0.0)) 0.0625 0.0625 0.0))
                      (schCreateWire sfyCv "draw" "full" (list (list 0.5 0.0) (list 0.5 0.5)) 0.0625 0.0625 0.0)
                      (fprintf sfyP "wire draw returned %d figures for 2 points\n" (length sfyWs))
                      (fprintf sfyP "wire 3-point returned %d figures\n"
                        (length (schCreateWire sfyCv "draw" "full"
                                  (list (list 2.0 0.0) (list 3.0 0.0) (list 3.0 1.0)) 0.0625 0.0625 0.0)))
                      (schCreateWire sfyCv "draw" "full" (list (list 4.0 0.0) (list 5.0 0.0)) 0.0625 0.0625 0.0)
                      (schCreateWire sfyCv "draw" "full" (list (list 4.5 -0.5) (list 4.5 0.5)) 0.0625 0.0625 0.0)
                      (setq sfyX (schSolder sfyCv (list 4.5 0.0)))
                      (fprintf sfyP "solder objType=%L lpp=%L\n" (and sfyX (getSGq sfyX objType)) (and sfyX (getSGq sfyX lpp)))
                      (schCreateWireLabel sfyCv (car sfyWs) (list 0.25 0.0) "n1" "lowerLeft" "R0" "stick" 0.0625 nil))
                    t)
            (fprintf sfyP "wires ERROR\n"))
          (unless (errset
                    (progn
                      (setq sfyM (dbOpenCellViewByType "basic" "ipin" "symbol" nil "r"))
                      (setq sfyX (and sfyM (schCreatePin sfyCv sfyM "pin_a" "input" nil (list -1.0 0.0) "R0")))
                      (fprintf sfyP "schCreatePin returns objType=%L name=%L purpose=%L\n" (and sfyX (getSGq sfyX objType))
                        (and sfyX (getSGq sfyX name)) (and sfyX (getSGq sfyX purpose))))
                    t)
            (fprintf sfyP "pin ERROR\n"))
          (unless (errset
                    (progn
                      (setq sfyM (dbOpenCellViewByType "analogLib" "res" "symbol" nil "r"))
                      (setq sfyInst (and sfyM (schCreateInst sfyCv sfyM "R0" (list 0.0 -2.0) "R0")))
                      (setq sfyCdf (and sfyInst (cdfGetInstCDF sfyInst)))
                      (setq sfyPar (and sfyCdf (cdfFindParamByName sfyCdf "r")))
                      (when sfyPar
                        (fprintf sfyP "inst props before %L\n" (getSGq (getSGq sfyInst prop) name))
                        (setSGq sfyPar (getSGq sfyPar defValue) value)
                        (fprintf sfyP "inst props after setting the default %L\n" (getSGq (getSGq sfyInst prop) name))
                        (setSGq sfyPar "2k" value)
                        (fprintf sfyP "inst props after 2k %L types %L\n" (getSGq (getSGq sfyInst prop) name)
                          (getSGq (getSGq sfyInst prop) valueType))))
                    t)
            (fprintf sfyP "instcdf ERROR\n"))
          ;; G13: the type an inherited-connection override gets; the open-mode spellings
          ;; SfyOpenForEdit and SfyPurgeWriting compare.
          (unless (errset
                    (progn
                      (fprintf sfyP "mode w-open %L\n" (getSGq sfyCv mode))
                      (when sfyInst
                        (dbReplaceProp sfyInst "sfyNet" "netSet" "VDDA")
                        (fprintf sfyP "netSet props %L\n"
                          (mapcar (lambda (sfyQ0) (list (getSGq sfyQ0 name) (getSGq sfyQ0 valueType)
                                                     (getSGq sfyQ0 value)))
                            (getSGq sfyInst prop)))))
                    t)
            (fprintf sfyP "netSet ERROR\n"))
          (drain sfyP)
          ;; G30, editable, callbacks: a PDK device (?pdkLib ?pdkCell).
          (when (and pdkLib pdkCell)
            (unless (errset
                      (progn
                        (setq sfyM (dbOpenCellViewByType pdkLib pdkCell "symbol" nil "r"))
                        (setq sfyX (and sfyM (schCreateInst sfyCv sfyM "MP0" (list 0.0 -6.0) "R0")))
                        (setq sfyCdf (and sfyX (cdfGetInstCDF sfyX)))
                        (fprintf sfyP "pdk %s/%s inst=%L cdf=%L\n" pdkLib pdkCell (and sfyX t) (and sfyCdf t))
                        (foreach sfyQ0 (and sfyCdf (getSGq sfyCdf parameters))
                          (fprintf sfyP "pdkparam %L %L editable=%L display=%L callback=%L value=%L\n"
                            (getSGq sfyQ0 name) (getSGq sfyQ0 paramType) (getSGq sfyQ0 editable)
                            (getSGq sfyQ0 display) (getSGq sfyQ0 callback) (getSGq sfyQ0 value)))
                        (fprintf sfyP "pdk props before %L\n" (getSGq (getSGq sfyX prop) name))
                        (drain sfyP)
                        (setq sfyPar (and sfyCdf (car (setof sfyQ0 (getSGq sfyCdf parameters)
                                                  (and (member (getSGq sfyQ0 name) (list "w" "fingers" "nf"))
                                                       (nequal (getSGq sfyQ0 editable) "nil")
                                                       (stringp (getSGq sfyQ0 callback)))))))
                        (when (and sfyPar (fboundp 'SfyCallbacks))
                          (fprintf sfyP "pdk callbacks of %L -> failed %L\n" (getSGq sfyPar name)
                            (SfyCallbacks sfyX (list (getSGq sfyPar name))))
                          (fprintf sfyP "pdk props after %L\n"
                            (mapcar (lambda (sfyQ0) (list (getSGq sfyQ0 name) (getSGq sfyQ0 value)))
                              (getSGq sfyX prop)))))
                      t)
              (fprintf sfyP "pdk ERROR %L\n" (getqq errset errset)))
            (drain sfyP))
          (unless (errset
                    (progn
                      (dbReplaceProp sfyCv "sfyProbeB" "boolean" t)
                      (fprintf sfyP "boolean prop reads %L\n" (getSGq sfyCv sfyProbeB))
                      (setq sfyX (dbCreateArc sfyCv (list "text" "drawing") (list (list 0.0 2.0) (list 1.0 3.0))
                                (list (list 0.5 2.5) (list 1.0 3.0))))
                      (when sfyX
                        (fprintf sfyP "arc created start=%L stop=%L\n" (getSGq sfyX startAngle) (getSGq sfyX stopAngle))
                        (setSGq sfyX 0.5 startAngle)
                        (setSGq sfyX 2.0 stopAngle)
                        (fprintf sfyP "arc set start=%L stop=%L bBox=%L\n" (getSGq sfyX startAngle)
                          (getSGq sfyX stopAngle) (getSGq sfyX bBox))))
                    t)
            (fprintf sfyP "arcprop ERROR\n"))
          (unless (errset
                    (progn
                      (fprintf sfyP "check %L\n" (schCheck sfyCv))
                      (foreach sfyQ0 (getSGq sfyCv prop)
                        (fprintf sfyP "cvprop %L %L\n" (getSGq sfyQ0 name) (getSGq sfyQ0 valueType)))
                      (foreach sfyF (getSGq sfyCv shapes)
                        (fprintf sfyP "shape %s %L parent=%L net=%L\n" (getSGq sfyF objType) (getSGq sfyF lpp)
                          (and (getSGq sfyF parent) (getSGq (getSGq sfyF parent) objType))
                          (and (getSGq sfyF net) (getSGq (getSGq sfyF net) name))))
                      (foreach sfyI (getSGq sfyCv instances)
                        (fprintf sfyP "instance %L %L purpose=%L\n" (getSGq sfyI name) (getSGq sfyI cellName) (getSGq sfyI purpose))))
                    t)
            (fprintf sfyP "check ERROR\n")))
        (dbPurge sfyCv)))
    (drain sfyP)

    ;; Maestro (G6, G18-G22, G28, G29): scratchLib/sfy_probe/maestro, created and saved.
    ;; isCallable, not fboundp: maestro functions load with the ADE context on first call.
    (unless (car (errset (isCallable 'maeOpenSetup)))
      (fprintf sfyP "mae skipped: maeOpenSetup is not callable\n"))
    (when (car (errset (isCallable 'maeOpenSetup)))
      ;; maeCreateTest refuses a design that does not exist (ASSEMBLER-8062 on a live run),
      ;; and the probe schematic above is purged: the test gets an empty saved one.
      (setq sfyM (car (errset (dbOpenCellViewByType sfyScratchLib "sfy_probe_tb" "schematic" "schematic" "w"))))
      (when sfyM
        (errset (schCheck sfyM))
        (fprintf sfyP "mae design saved %L\n" (car (errset (dbSave sfyM))))
        (errset (dbClose sfyM)))
      (setq sfySess (car (errset (maeOpenSetup sfyScratchLib "sfy_probe" "maestro" ?mode "a"))))
      (fprintf sfyP "mae open a %L\n" sfySess)
      (when sfySess
        (setq sfySdb (car (errset (axlGetMainSetupDB sfySess))))
        (fprintf sfyP "mae createTest %L\n"
          (errset (maeCreateTest "T" ?lib sfyScratchLib ?cell "sfy_probe_tb" ?view "schematic"
                    ?simulator "spectre" ?session sfySess) t))
        (fprintf sfyP "mae setAnalysis tran %L dc %L ac %L\n"
          (errset (maeSetAnalysis "T" "tran" ?enable t ?options (list (list "stop" "1n")) ?session sfySess) t)
          (errset (maeSetAnalysis "T" "dc" ?enable t ?options (list (list "saveOppoint" t)) ?session sfySess) t)
          (errset (maeSetAnalysis "T" "ac" ?enable t
                    ?options (list (list "start" "1") (list "stop" "1G") (list "incrType" "Logarithmic")
                               (list "stepTypeLog" "Points Per Decade") (list "dec" "10"))
                    ?session sfySess) t))
        (foreach sfyA (list "tran" "dc" "ac")
          (fprintf sfyP "mae getAnalysis %s %L\n" sfyA (errset (maeGetAnalysis "T" sfyA ?session sfySess) t)))
        (fprintf sfyP "mae enabled %L\n" (errset (maeGetEnabledAnalysis "T" ?session sfySess) t))
        (fprintf sfyP "mae setVar %L testVar %L\n"
          (errset (maeSetVar "VDD" "1" ?session sfySess) t)
          (errset (maeSetVar "VDD" "2" ?typeName "test" ?typeValue (list "T") ?session sfySess) t))
        (foreach sfyTy (list "variables" "corners" "tests")
          (fprintf sfyP "mae getSetup %s %L\n" sfyTy (errset (maeGetSetup ?typeName sfyTy ?session sfySess) t)))
        (fprintf sfyP "mae getSetup %L\n" (errset (maeGetSetup ?session sfySess) t))
        (fprintf sfyP "mae testVars %L\n"
          (errset (let ((sfyTh (axlGetTest sfySdb "T")))
                    (list sfyTh (axlGetVars sfyTh) (axlGetVarValue (axlGetVar sfyTh "VDD")))) t))
        (fprintf sfyP "mae setSimOption %L\n"
          (errset (maeSetSimOption "T" ?options (list (list "temp" "55")) ?session sfySess) t))
        (fprintf sfyP "mae getSimOption temp %L all %L\n"
          (errset (maeGetSimOption "T" ?option "temp" ?session sfySess) t)
          (errset (maeGetSimOption "T" ?session sfySess) t))
        (fprintf sfyP "mae addOutput point %L net %L\n"
          (errset (maeAddOutput "o1" "T" ?outputType "point" ?expr "ymax(VT(\"/n1\"))" ?session sfySess) t)
          (errset (maeAddOutput "o2" "T" ?outputType "net" ?signalName "/n1" ?session sfySess) t))
        (foreach sfySo (car (errset (maeGetTestOutputs "T" ?session sfySess)))
          (fprintf sfyP "mae output name=%L expression=%L expr=%L signal=%L signalName=%L outputType=%L\n"
            (errset (getSGq sfySo name)) (errset (getSGq sfySo expression)) (errset (getSGq sfySo expr))
            (errset (getSGq sfySo signal)) (errset (getSGq sfySo signalName)) (errset (getSGq sfySo outputType))))
        (fprintf sfyP "mae setSpec range %L\n"
          (errset (maeSetSpec "o1" "T" ?range (list "1" "2") ?session sfySess) t))
        (fprintf sfyP "mae specData %L\n" (errset (axlGetSpecData sfySdb "o1" "T") t))
        (fprintf sfyP "mae setSpec gt %L\n" (errset (maeSetSpec "o1" "T" ?gt "1" ?session sfySess) t))
        (fprintf sfyP "mae specData %L\n" (errset (axlGetSpecData sfySdb "o1" "T") t))
        (fprintf sfyP "mae setCorner %L\n" (errset (maeSetCorner "C1" ?enabled t ?session sfySess) t))
        (fprintf sfyP "mae corner models %L\n"
          (errset (let ((sfyC (axlGetCorner sfySdb "C1")) (sfyMh nil))
                    (setq sfyMh (axlPutModel sfyC "m.scs"))
                    (axlSetModelFile sfyMh "/tmp/m.scs")
                    (axlSetModelSection sfyMh "tt")
                    (list (axlGetModels sfyC) (axlGetModelSection (axlGetModel sfyC "m.scs"))
                          (axlGetModelFile (axlGetModel sfyC "m.scs")))) t))
        (fprintf sfyP "mae setEnvOption %L\n"
          (errset (maeSetEnvOption "T" ?options (list (list "modelFiles" (list (list "/tmp/m.scs" "tt"))))
                    ?session sfySess) t))
        (fprintf sfyP "mae getEnvOption modelFiles %L definitionFiles %L\n"
          (errset (maeGetEnvOption "T" ?option "modelFiles" ?session sfySess) t)
          (errset (maeGetEnvOption "T" ?option "definitionFiles" ?session sfySess) t))
        (fprintf sfyP "mae save %L\n"
          (errset (maeSaveSetup ?lib sfyScratchLib ?cell "sfy_probe" ?view "maestro" ?session sfySess) t))
        (fprintf sfyP "mae close forceClose %L\n"
          (errset (maeCloseSession ?session sfySess ?forceClose t) t))
        (drain sfyP)
        (setq sfySess (car (errset (maeOpenSetup sfyScratchLib "sfy_probe" "maestro" ?mode "r"))))
        (fprintf sfyP "mae reopen r %L tests %L\n" sfySess
          (and sfySess (errset (maeGetSetup ?session sfySess) t)))
        (when sfySess
          (fprintf sfyP "mae close %L\n" (errset (maeCloseSession ?session sfySess ?forceClose t) t)))))
    (fprintf sfyP "end\n")
    (close sfyP)
    t))
