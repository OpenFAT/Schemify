;; Test fixtures: mock Virtuoso libraries built through the Cadence API the mock implements
;; (tests/skill/mockdb.rs). Not shipped. Coordinates are written in database units
;; (160 per inch) and converted with SfyFixP.
;;
;; Libraries (added to cds.lib by the test first): analogLib, basic, pdk, cb, work.

(procedure (SfyFixU v) (quotient (float v) 160.0))
(procedure (SfyFixP x y) (list (SfyFixU x) (SfyFixU y)))
(procedure (SfyFixBox x0 y0 x1 y1) (list (SfyFixP x0 y0) (SfyFixP x1 y1)))

(procedure (SfyFixOpen lib cell view vtype)
  (let ((cv (dbOpenCellViewByType lib cell view vtype "w")))
    (unless cv (error "fixture: cannot open %s/%s/%s" lib cell view))
    cv))

(procedure (SfyFixSave cv)
  (unless (dbSave cv) (error "fixture: cannot save"))
  (dbClose cv))

;; A symbol: box body, selection box, cdsName() label, and one 4x4 pin per (name dir x y)
;; with its pin-name label glued to the pin figure.
(procedure (SfyFixSymbol lib cell pins)
  (let ((cv (SfyFixOpen lib cell "symbol" "schematicSymbol")) (net nil) (term nil) (fig nil) (lbl nil))
    (dbCreateRect cv (list "device" "drawing") (SfyFixBox -20 -20 20 20))
    (dbCreateRect cv (list "instance" "drawing") (SfyFixBox -40 -40 40 40))
    (foreach p pins
      (setq net (dbMakeNet cv (car p)))
      (setq term (dbCreateTerm net (car p) (cadr p)))
      (setq fig (dbCreateRect cv (list "pin" "drawing")
                  (SfyFixBox (difference (caddr p) 2) (difference (cadddr p) 2)
                             (plus (caddr p) 2) (plus (cadddr p) 2))))
      (dbCreatePin net fig nil term)
      (setq lbl (dbCreateLabel cv (list "pin" "label") (SfyFixP (caddr p) (cadddr p)) (car p)
                  "centerLeft" "R0" "stick" (SfyFixU 10)))
      (schGlueLabel lbl fig))
    (setq lbl (dbCreateLabel cv (list "annotate" "drawing7") (SfyFixP 0 30) "cdsName()"
                "lowerLeft" "R0" "stick" (SfyFixU 10)))
    (setSGq lbl "ILLabel" labelType)
    (SfyFixSave cv)))

(procedure (SfyFixCdf lib cell)
  (let ((cdf (cdfCreateBaseCellCDF (ddGetObj lib cell))))
    (unless cdf (error "fixture: no CDF for %s/%s" lib cell))
    cdf))

(procedure (SfyFixMos lib cell)
  (let ((cdf nil))
    (SfyFixSymbol lib cell (list (list "D" "inputOutput" 0 40) (list "G" "input" -40 0)
                             (list "S" "inputOutput" 0 -40) (list "B" "inputOutput" 40 0)))
    (setq cdf (SfyFixCdf lib cell))
    (cdfCreateParam cdf ?name "model" ?type "string" ?defValue cell ?prompt "Model name"
      ?storeDefault "yes")
    (cdfCreateParam cdf ?name "w" ?type "string" ?defValue "1u" ?prompt "Width"
      ?units "lengthMetric" ?parseAsNumber "yes" ?parseAsCEL "yes")
    (cdfCreateParam cdf ?name "l" ?type "string" ?defValue "180n" ?prompt "Length"
      ?units "lengthMetric" ?parseAsNumber "yes" ?parseAsCEL "yes")
    (cdfCreateParam cdf ?name "m" ?type "string" ?defValue "1" ?prompt "Multiplier"
      ?parseAsNumber "yes" ?parseAsCEL "yes")
    (setSGq cdf "-model l w" paramLabelSet)
    (setSGq cdf
      (list nil
        'spectre (list nil 'termOrder '(D G S B) 'instParameters '(w l m) 'otherParameters '(model)
                   'termMapping (list nil 'D (stringToSymbol ":d") 'G (stringToSymbol ":g")
                                  'S (stringToSymbol ":s") 'B (stringToSymbol ":b")))
        'auCdl (list nil 'netlistProcedure 'ansCdlCompPrim 'instParameters '(m L W)
                 'componentName "nmos" 'termOrder '(D G S B) 'propMapping '(nil L l W w)
                 'namePrefix "M" 'dollarParams nil))
      simInfo)
    (cdfSaveCDF cdf)))

;; analogLib (res, nmos4, pmos4, gnd, vdd) and basic (pin masters, noConn).
(procedure (SfyFixStdlib)
  (let ((cdf nil))
    (SfyFixSymbol "analogLib" "res" (list (list "PLUS" "inputOutput" 0 40) (list "MINUS" "inputOutput" 0 -40)))
    (setq cdf (SfyFixCdf "analogLib" "res"))
    (cdfCreateParam cdf ?name "r" ?type "string" ?defValue "1K" ?prompt "Resistance"
      ?units "resistance" ?parseAsNumber "yes" ?parseAsCEL "yes")
    (cdfCreateParam cdf ?name "m" ?type "string" ?defValue "1" ?prompt "Multiplier"
      ?parseAsNumber "yes" ?parseAsCEL "yes")
    (setSGq cdf
      (list nil 'spectre (list nil 'termOrder '(PLUS MINUS) 'instParameters '(r m)
                           'componentName "resistor")
                'auCdl (list nil 'netlistProcedure 'ansCdlCompPrim 'instParameters '(R)
                         'propMapping '(nil R r) 'namePrefix "R"))
      simInfo)
    (cdfSaveCDF cdf)
    (SfyFixMos "analogLib" "nmos4")
    (SfyFixMos "analogLib" "pmos4")
    (SfyFixSymbol "analogLib" "gnd" (list (list "gnd!" "inputOutput" 0 0)))
    (SfyFixSymbol "analogLib" "vdd" (list (list "vdd!" "inputOutput" 0 0)))
    (SfyFixSymbol "basic" "ipin" nil)
    (SfyFixSymbol "basic" "opin" nil)
    (SfyFixSymbol "basic" "iopin" nil)
    (SfyFixSymbol "basic" "noConn" (list (list "noConn" "inputOutput" 0 0)))
    t))

;; pdk/nch: a PDK device with a parameter of every CDF type and two simulators; pdk/padonly:
;; a cell without a symbol (not dumped).
(procedure (SfyFixPdk)
  (let ((cdf nil))
    (SfyFixSymbol "pdk" "nch" (list (list "D" "inputOutput" 0 40) (list "G" "input" -40 0)
                                (list "S" "inputOutput" 0 -40) (list "B" "inputOutput" 40 0)))
    (setq cdf (SfyFixCdf "pdk" "nch"))
    (cdfCreateParam cdf ?name "model" ?type "string" ?defValue "nch_mac" ?prompt "Model"
      ?storeDefault "yes" ?display "nil")
    (cdfCreateParam cdf ?name "w" ?type "string" ?defValue "1u" ?prompt "Width" ?units "lengthMetric"
      ?parseAsNumber "yes" ?parseAsCEL "yes" ?callback "pdkWidthCB()")
    (cdfCreateParam cdf ?name "nf" ?type "int" ?defValue 1 ?prompt "Fingers")
    (cdfCreateParam cdf ?name "mult" ?type "float" ?defValue 1.5 ?prompt "Multiplier")
    (cdfCreateParam cdf ?name "bodytie" ?type "boolean" ?prompt "Body tie")
    (cdfCreateParam cdf ?name "layout" ?type "cyclic" ?defValue "a" ?choices '("a" "b") ?prompt "Layout")
    (cdfCreateParam cdf ?name "region" ?type "radio" ?defValue "on" ?choices '("on" "off") ?prompt "Region"
      ?dontSave "t")
    (cdfCreateParam cdf ?name "refresh" ?type "button" ?prompt "Refresh")
    (cdfCreateParam cdf ?name "sub" ?type "netSet" ?defValue "gnd!" ?prompt "Substrate" ?storeDefault "yes")
    (setSGq cdf "-model l w" paramLabelSet)
    (setSGq cdf
      (list nil
        'spectre (list nil 'termOrder '(D G S B) 'instParameters '(w nf) 'otherParameters '(model)
                   'termMapping (list nil 'D (stringToSymbol ":d") 'B "(FUNCTION minus(root(\"D\")))"))
        'auCdl (list nil 'netlistProcedure 'ansCdlCompPrim 'instParameters '(m L W)
                 'componentName "nmos" 'termOrder '(D G S B) 'propMapping '(nil L l W w)
                 'namePrefix "M" 'modelName "NCH" 'dollarParams nil))
      simInfo)
    (cdfSaveCDF cdf)
    (ddGetObj "pdk" "padonly" nil nil nil "a")
    t))

;; work/inv: schematic (pmos4, nmos4, wires, wire labels, pins, globals, props, note text
;; and shape, CDF values) and symbol.
(procedure (SfyFixInv)
  (let ((cv nil) (sym nil) (m nil) (p0 nil) (n0 nil) (w nil) (pin nil) (cdf nil))
    (SfyFixSymbol "work" "inv" (list (list "in" "input" -40 0) (list "out" "output" 40 0)))
    (setq cv (SfyFixOpen "work" "inv" "schematic" "schematic"))
    (setq p0 (schCreateInst cv (dbOpenCellViewByType "analogLib" "pmos4" "symbol" nil "r") "P0"
               (SfyFixP 0 80) "R0"))
    (setq n0 (schCreateInst cv (dbOpenCellViewByType "analogLib" "nmos4" "symbol" nil "r") "N0"
               (SfyFixP 0 -80) "MX"))
    (schCreateInst cv (dbOpenCellViewByType "analogLib" "vdd" "symbol" nil "r") "V0" (SfyFixP 0 160) "R0")
    (schCreateInst cv (dbOpenCellViewByType "analogLib" "gnd" "symbol" nil "r") "G0" (SfyFixP 0 -160) "R0")
    (setq cdf (cdfGetInstCDF p0))
    (setSGq (cdfFindParamByName cdf "w") "2u" value)
    (setq cdf (cdfGetInstCDF n0))
    (setSGq (cdfFindParamByName cdf "l") "200n" value)
    (dbReplaceProp p0 "schemifyId" "string" "i0001")
    (dbReplaceProp n0 "schemifyId" "string" "i0002")
    (setq w (car (schCreateWire cv "draw" "full" (list (SfyFixP 0 40) (SfyFixP 0 -40)) 0.0625 0.0625 0.0)))
    (schCreateWireLabel cv w (SfyFixP 0 10) "out" "lowerLeft" "R90" "stick" (SfyFixU 10) nil)
    (dbReplaceProp w "schemifyId" "string" "w0001")
    (setq w (car (schCreateWire cv "draw" "full" (list (SfyFixP -40 80) (SfyFixP -40 -80)) 0.0625 0.0625 0.0)))
    (schCreateWireLabel cv w (SfyFixP -40 20) "in" "lowerLeft" "R90" "stick" (SfyFixU 10) nil)
    (setq w (car (schCreateWire cv "draw" "full" (list (SfyFixP 0 120) (SfyFixP 0 160)) 0.0625 0.0625 0.0)))
    (schCreateWireLabel cv w (SfyFixP 0 140) "vdd!" "lowerLeft" "R0" "stick" (SfyFixU 10) nil)
    (setq w (car (schCreateWire cv "draw" "full" (list (SfyFixP 0 -120) (SfyFixP 0 -160)) 0.0625 0.0625 0.0)))
    (schCreateWireLabel cv w (SfyFixP 0 -140) "gnd!" "upperLeft" "R0" "stick" (SfyFixU 10) nil)
    (schCreateWire cv "draw" "full" (list (SfyFixP -80 0) (SfyFixP -40 0)) 0.0625 0.0625 0.0)
    (schCreateWire cv "draw" "full" (list (SfyFixP 0 0) (SfyFixP 80 0)) 0.0625 0.0625 0.0)
    (schCreatePin cv (dbOpenCellViewByType "basic" "ipin" "symbol" nil "r") "in" "input" nil (SfyFixP -80 0) "R0")
    (schCreatePin cv (dbOpenCellViewByType "basic" "opin" "symbol" nil "r") "out" "output" nil (SfyFixP 80 0) "R0")
    (schCreateNoteLabel cv (SfyFixP -60 200) "CMOS inverter" "lowerLeft" "R0" "stick" (SfyFixU 12) "normalLabel")
    (dbCreateRect cv (list "text" "drawing1") (SfyFixBox -100 -200 100 220))
    (dbReplaceProp cv "schemifyRecords1" "string" "prose \"inverter\"\nvar k 2")
    (dbReplaceProp cv "author" "string" "someone")
    (schCheck cv)
    (SfyFixSave cv)))

;; work/bus: a bus (wide wire) with a vector label, an iterated instance, a net-expression
;; label and an alias label; schematic only.
(procedure (SfyFixBus)
  (let ((cv (SfyFixOpen "work" "bus" "schematic" "schematic")) (w nil) (r nil))
    (setq w (car (schCreateWire cv "draw" "full" (list (SfyFixP 0 0) (SfyFixP 200 0)) 0.0625 0.0625 (SfyFixU 5))))
    (schCreateWireLabel cv w (SfyFixP 100 0) "d<3:0>" "lowerCenter" "R0" "stick" (SfyFixU 10) nil)
    (setq r (schCreateInst cv (dbOpenCellViewByType "analogLib" "res" "symbol" nil "r") "R<3:0>"
              (SfyFixP 100 -100) "R90"))
    (setSGq (cdfFindParamByName (cdfGetInstCDF r) "r") "10k" value)
    (setq w (car (schCreateWire cv "draw" "full" (list (SfyFixP 0 100) (SfyFixP 0 200)) 0.0625 0.0625 0.0)))
    (schCreateNetExpression cv "[@vdd:%:vdd!]" w (SfyFixP 0 150) "lowerLeft" "R90" "stick" (SfyFixU 10))
    (schCreateWireLabel cv w (SfyFixP 0 180) "supply" "lowerLeft" "R90" "stick" (SfyFixU 10) t)
    (setq w (car (schCreateWire cv "draw" "full" (list (SfyFixP 100 -140) (SfyFixP 100 -200)) 0.0625 0.0625 0.0)))
    (schCreateWireLabel cv w (SfyFixP 100 -180) "d<0>" "lowerLeft" "R0" "stick" (SfyFixU 10) nil)
    (schCheck cv)
    (SfyFixSave cv)))

;; work/symkit: a symbol with every shape kind and label kind, escapes and UTF-8 text.
(procedure (SfyFixSymkit)
  (let ((cv (SfyFixOpen "work" "symkit" "symbol" "schematicSymbol")) (net nil) (term nil) (fig nil)
        (a nil) (l nil))
    (dbCreateLine cv (list "device" "drawing") (list (SfyFixP -40 0) (SfyFixP -20 0) (SfyFixP -20 20)))
    (dbCreateRect cv (list "device" "drawing") (SfyFixBox -20 -20 20 20))
    (dbCreatePolygon cv (list "device" "drawing1") (list (SfyFixP -10 -10) (SfyFixP 10 0) (SfyFixP -10 10)))
    (dbCreateEllipse cv (list "device" "drawing") (SfyFixBox 20 -5 30 5))
    (dbCreateEllipse cv (list "device" "drawing") (SfyFixBox -30 30 -10 40))
    (setq a (dbCreateArc cv (list "device" "drawing") (SfyFixBox -10 -30 10 -10) (SfyFixBox -10 -30 10 -10)))
    (setSGq a 0.0 startAngle)
    (setSGq a 1.5707963267948966 stopAngle)
    (dbCreatePath cv (list "device" "drawing1") (list (SfyFixP 30 0) (SfyFixP 40 0)) (SfyFixU 5))
    (dbCreateRect cv (list "instance" "drawing") (SfyFixBox -50 -50 50 50))
    (setq net (dbMakeNet cv "A"))
    (setq term (dbCreateTerm net "A" "input"))
    (setq fig (dbCreateRect cv (list "pin" "drawing") (SfyFixBox -42 -2 -38 2)))
    (dbCreatePin net fig nil term)
    (setq fig (dbCreateRect cv (list "pin" "drawing") (SfyFixBox -42 18 -38 22)))
    (dbCreatePin net fig nil term)
    (setq l (dbCreateLabel cv (list "pin" "label") (SfyFixP -36 0) "A" "centerLeft" "R0" "stick" (SfyFixU 8)))
    (schGlueLabel l fig)
    (setq net (dbMakeNet cv "Y"))
    (setq term (dbCreateTerm net "Y" "output"))
    (setq fig (dbCreateRect cv (list "pin" "drawing") (SfyFixBox 38 -2 42 2)))
    (dbCreatePin net fig nil term)
    (setq l (dbCreateLabel cv (list "instance" "label") (SfyFixP 0 40) "[@instanceName]" "lowerCenter" "R0" "stick" (SfyFixU 10)))
    (setSGq l "NLPLabel" labelType)
    (setq l (dbCreateLabel cv (list "device" "label") (SfyFixP 0 -40) "[@partName]" "upperCenter" "R0" "stick" (SfyFixU 10)))
    (setSGq l "NLPLabel" labelType)
    (setq l (dbCreateLabel cv (list "annotate" "drawing") (SfyFixP 30 30) "cdsParam(1)" "lowerLeft" "R90" "roman" (SfyFixU 8)))
    (setSGq l "ILLabel" labelType)
    (dbCreateLabel cv (list "annotate" "drawing") (SfyFixP -30 -45) "a\"b\\c\td\001 \316\274A" "lowerLeft" "MY" "stick" (SfyFixU 8))
    (dbReplaceProp cv "schemifyRecords1" "string" "doc \"every shape\"")
    (dbReplaceProp fig "schemifyId" "string" "p0002")
    (SfyFixSave cv)))

;; work/netx: a symbol whose terminal carries an inherited-connection net expression, with
;; a base CDF and a schematic that uses it.
(procedure (SfyFixNetx)
  (let ((cv (SfyFixOpen "work" "netx" "symbol" "schematicSymbol")) (net nil) (term nil) (fig nil)
        (cdf nil) (w nil))
    (dbCreatePolygon cv (list "device" "drawing") (list (SfyFixP -20 -20) (SfyFixP 20 0) (SfyFixP -20 20)))
    (foreach p (list (list "a" "input" -40 0 "") (list "y" "output" 40 0 "")
                 (list "vdd" "inputOutput" 0 40 "[@vdd:%:vdd!]"))
      (setq net (dbMakeNet cv (car p)))
      (setq term (dbCreateTerm net (car p) (cadr p)))
      (unless (equal (nth 4 p) "") (dbCreateTermNetExpr term (nth 4 p)))
      (setq fig (dbCreateRect cv (list "pin" "drawing")
                  (SfyFixBox (difference (caddr p) 2) (difference (cadddr p) 2)
                             (plus (caddr p) 2) (plus (cadddr p) 2))))
      (dbCreatePin net fig nil term))
    (SfyFixSave cv)
    (setq cdf (SfyFixCdf "work" "netx"))
    (cdfCreateParam cdf ?name "gain" ?type "string" ?defValue "2" ?prompt "Gain" ?parseAsNumber "yes" ?parseAsCEL "yes")
    (setSGq cdf (list nil 'spectre (list nil 'termOrder '(a y vdd))) simInfo)
    (cdfSaveCDF cdf)
    (setq cv (SfyFixOpen "work" "netx" "schematic" "schematic"))
    (setq w (car (schCreateWire cv "draw" "full" (list (SfyFixP 0 40) (SfyFixP 0 80)) 0.0625 0.0625 0.0)))
    (schCreateNetExpression cv "[@vdd:%:vdd!]" w (SfyFixP 0 60) "lowerLeft" "R90" "stick" (SfyFixU 10))
    (schCreatePin cv (dbOpenCellViewByType "basic" "ipin" "symbol" nil "r") "a" "input" nil (SfyFixP -80 0) "R0")
    (schCreatePin cv (dbOpenCellViewByType "basic" "opin" "symbol" nil "r") "y" "output" nil (SfyFixP 80 0) "R180")
    (schCreatePin cv (dbOpenCellViewByType "basic" "iopin" "symbol" nil "r") "vdd" "inputOutput" nil (SfyFixP 0 80) "R90")
    (schCheck cv)
    (SfyFixSave cv)))

;; work/vamod: a symbol plus a veriloga text view longer than one chunk and a functional view.
(procedure (SfyFixVamod)
  (SfyFixSymbol "work" "vamod" (list (list "p" "inputOutput" 0 40) (list "n" "inputOutput" 0 -40)))
  t)

;; cb/dev: a device whose CDF callbacks record their order (c, a, b in CDF order); c's
;; callback fails; a's records what it sees (b's value, the type of cdfgData->id); b's
;; clamps b to "5" and derives the read-only d as "9".
(procedure (SfyFixCb)
  (let ((cdf nil))
    (SfyFixSymbol "cb" "dev" (list (list "p" "inputOutput" 0 40) (list "n" "inputOutput" 0 -40)))
    (setq cdf (SfyFixCdf "cb" "dev"))
    (cdfCreateParam cdf ?name "c" ?type "string" ?defValue "0" ?prompt "C"
      ?callback "(progn (setq sfyCbOrder (cons \"c\" sfyCbOrder)) (error \"c refuses\"))")
    (cdfCreateParam cdf ?name "a" ?type "string" ?defValue "0" ?prompt "A"
      ?callback "(progn (setq sfyCbOrder (cons \"a\" sfyCbOrder)) (setq sfyCbSeen (list (getSGq (getSGq cdfgData b) value) (getSGq (getSGq cdfgData id) objType))))")
    (cdfCreateParam cdf ?name "b" ?type "string" ?defValue "0" ?prompt "B"
      ?callback "(progn (setq sfyCbOrder (cons \"b\" sfyCbOrder)) (setSGq (getSGq cdfgData b) \"5\" value) (setSGq (getSGq cdfgData d) \"9\" value))")
    (cdfCreateParam cdf ?name "d" ?type "string" ?defValue "0" ?prompt "D" ?editable "nil")
    (cdfCreateParam cdf ?name "n" ?type "int" ?defValue 0 ?prompt "N")
    (cdfSaveCDF cdf)
    t))

(procedure (SfyFixAll)
  (SfyFixStdlib)
  (SfyFixPdk)
  (SfyFixCb)
  (SfyFixInv)
  (SfyFixBus)
  (SfyFixSymkit)
  (SfyFixNetx)
  (SfyFixVamod)
  t)
