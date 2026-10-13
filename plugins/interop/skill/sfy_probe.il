;; sfy_probe.il: the first-contact probe (interop spec 5.5, research virtuoso-skill-db 9).
;; Not part of schemify.il. In a real Virtuoso:
;;   (load "/abs/path/schemify.il") (load "/abs/path/sfy_probe.il") (SfyProbe "scratchLib")
;; then send /tmp/sfy_probe.txt. It touches only scratchLib/sfy_probe/schematic, opened "w"
;; and purged, never saved. Each section is trapped on its own; a failing section prints
;; "<section> ERROR" and the probe goes on. Every line names the GUESS (spec 13) it settles.

(procedure (SfyProbe scratchLib)
  (let ((p (outfile "/tmp/sfy_probe.txt" "w")) (cv nil) (sym nil) (ws nil) (x nil) (m nil)
        (inst nil) (cdf nil) (par nil) (s nil) (q nil) (r nil))
    (unless p (error "SfyProbe: cannot write /tmp/sfy_probe.txt"))
    (fprintf p "sfy_probe 1 scratch %s\n" scratchLib)

    ;; G9: printing and rounding.
    (unless (errset
              (progn
                (fprintf p "print L-string %L\n" "a\"b\\c\nd\001e")
                (fprintf p "print float L=%L g17=%s\n" (plus 0.1 0.2) (sprintf nil "%.17g" (plus 0.1 0.2)))
                (fprintf p "print round %d %d %d\n" (round 2.5) (round -2.5) (round (times 0.0625 160.0))))
              t)
      (fprintf p "print ERROR\n"))

    ;; G1: orientations, the image of (1 2).
    (unless (errset
              (foreach o (list "R0" "R90" "R180" "R270" "MY" "MYR90" "MX" "MXR90")
                (fprintf p "orient %s %L\n" o (dbTransformPoint (list 1.0 2.0) (list (list 0.0 0.0) o))))
              t)
      (fprintf p "orient ERROR\n"))

    ;; Name expansion (an oracle for the Rust name parser).
    (unless (errset (fprintf p "names %L\n" (dbProduceMemName "A<2:0>,<*2>(b,c),d<0:4:2*2>,vdd!")) t)
      (fprintf p "names ERROR\n"))

    ;; G12 and the text chunker: characters or bytes, signed or unsigned, byte order.
    (unless (errset
              (progn
                (fprintf p "string parseString-utf8 %d\n" (length (parseString "\303\251x" "")))
                (fprintf p "string charToInt-high %d\n"
                  (charToInt (stringToSymbol (car (parseString "\303\251" "")))))
                (fprintf p "string alphalessp B<a %L z<e-acute %L\n"
                  (alphalessp "B" "a") (alphalessp "z" "\303\251")))
              t)
      (fprintf p "string ERROR\n"))

    ;; G9: the escaper round trip over every byte 1..127 and a UTF-8 character.
    (unless (errset
              (when (fboundp 'SfyPutStr)
                (setq x nil)
                (for i 1 127 (setq x (cons (symbolToString (intToChar i)) x)))
                (setq s (strcat (buildString (reverse x) "") "\303\251"))
                (setq q (outfile "/tmp/sfy_probe_esc.sfyx" "w"))
                (fprintf q "(")
                (SfyPutStr q s)
                (fprintf q ")\n")
                (close q)
                (setq q (infile "/tmp/sfy_probe_esc.sfyx"))
                (setq r (lineread q))
                (close q)
                (fprintf p "escaper %s\n" (if (equal (car (car r)) s) "PASS" "FAIL")))
              t)
      (fprintf p "escaper ERROR\n"))

    ;; G3: analogLib global symbols and basic pin masters.
    (unless (errset
              (progn
                (foreach c (list "gnd" "vdd")
                  (setq sym (dbOpenCellViewByType "analogLib" c "symbol" nil "r"))
                  (fprintf p "global %s terminals %L\n" c (and sym (getSGq (getSGq sym terminals) name)))
                  (when sym (dbClose sym)))
                (foreach c (list "ipin" "opin" "iopin" "noConn")
                  (fprintf p "basic %s views %L\n" c
                    (getSGq (getSGq (ddGetObj "basic" c) views) name))))
              t)
      (fprintf p "global ERROR\n"))

    ;; G2 and a reference symbol's anatomy.
    (unless (errset
              (progn
                (setq sym (dbOpenCellViewByType "analogLib" "nmos4" "symbol" nil "r"))
                (when sym
                  (fprintf p "symbol units=%L dbu=%L type=%L\n" (getSGq sym userUnits)
                    (getSGq sym DBUPerUU) (getSGq sym cellViewType))
                  (foreach f (getSGq sym shapes)
                    (fprintf p "symshape %s %L %L %L pin=%L\n" (getSGq f objType) (getSGq f lpp)
                      (if (equal (getSGq f objType) "label") (getSGq f theLabel) (getSGq f bBox))
                      (getSGq f labelType) (and (getSGq f pin) (getSGq (getSGq f pin) name))))
                  (foreach t0 (getSGq sym terminals)
                    (fprintf p "symterm %s %s netexpr=%L pins=%L\n" (getSGq t0 name) (getSGq t0 direction)
                      (dbGetTermNetExpr t0) (getSGq (getSGq (getSGq t0 pins) fig) bBox)))
                  (dbClose sym)))
              t)
      (fprintf p "symbol ERROR\n"))

    ;; CDF attribute spellings (flags yes/t, display, defValue types) and simInfo.
    (unless (errset
              (progn
                (setq cdf (cdfGetBaseCellCDF (ddGetObj "analogLib" "res")))
                (when cdf
                  (fprintf p "cdf type=%L paramLabelSet=%L\n" (getSGq cdf type) (getSGq cdf paramLabelSet))
                  (foreach q0 (getSGq cdf parameters)
                    (fprintf p "cdfparam %L %L def=%L num=%L cel=%L store=%L display=%L dontSave=%L\n"
                      (getSGq q0 name) (getSGq q0 paramType) (getSGq q0 defValue) (getSGq q0 parseAsNumber)
                      (getSGq q0 parseAsCEL) (getSGq q0 storeDefault) (getSGq q0 display) (getSGq q0 dontSave)))
                  (fprintf p "cdf simInfo %L\n" (getSGq cdf simInfo))))
              t)
      (fprintf p "cdf ERROR\n"))

    ;; The scratch schematic: G2, G4, G5, G10, G11, G13, G14, G15 and boolean properties.
    (setq cv (dbOpenCellViewByType scratchLib "sfy_probe" "schematic" "schematic" "w"))
    (if (null cv)
      (fprintf p "scratch ERROR cannot open %s/sfy_probe/schematic for writing\n" scratchLib)
      (unwindProtect
        (progn
          (unless (errset
                    (progn
                      (fprintf p "sch units=%L dbu=%L\n" (getSGq cv userUnits) (getSGq cv DBUPerUU))
                      (setq ws (schCreateWire cv "draw" "full" (list (list 0.0 0.0) (list 1.0 0.0)) 0.0625 0.0625 0.0))
                      (schCreateWire cv "draw" "full" (list (list 0.5 0.0) (list 0.5 0.5)) 0.0625 0.0625 0.0)
                      (fprintf p "wire draw returned %d figures for 2 points\n" (length ws))
                      (fprintf p "wire 3-point returned %d figures\n"
                        (length (schCreateWire cv "draw" "full"
                                  (list (list 2.0 0.0) (list 3.0 0.0) (list 3.0 1.0)) 0.0625 0.0625 0.0)))
                      (schCreateWire cv "draw" "full" (list (list 4.0 0.0) (list 5.0 0.0)) 0.0625 0.0625 0.0)
                      (schCreateWire cv "draw" "full" (list (list 4.5 -0.5) (list 4.5 0.5)) 0.0625 0.0625 0.0)
                      (setq x (schSolder cv (list 4.5 0.0)))
                      (fprintf p "solder objType=%L lpp=%L\n" (and x (getSGq x objType)) (and x (getSGq x lpp)))
                      (schCreateWireLabel cv (car ws) (list 0.25 0.0) "n1" "lowerLeft" "R0" "stick" 0.0625 nil))
                    t)
            (fprintf p "wires ERROR\n"))
          (unless (errset
                    (progn
                      (setq m (dbOpenCellViewByType "basic" "ipin" "symbol" nil "r"))
                      (setq x (and m (schCreatePin cv m "pin_a" "input" nil (list -1.0 0.0) "R0")))
                      (fprintf p "schCreatePin returns objType=%L name=%L purpose=%L\n" (and x (getSGq x objType))
                        (and x (getSGq x name)) (and x (getSGq x purpose))))
                    t)
            (fprintf p "pin ERROR\n"))
          (unless (errset
                    (progn
                      (setq m (dbOpenCellViewByType "analogLib" "res" "symbol" nil "r"))
                      (setq inst (and m (schCreateInst cv m "R0" (list 0.0 -2.0) "R0")))
                      (setq cdf (and inst (cdfGetInstCDF inst)))
                      (setq par (and cdf (cdfFindParamByName cdf "r")))
                      (when par
                        (fprintf p "inst props before %L\n" (getSGq (getSGq inst prop) name))
                        (setSGq par (getSGq par defValue) value)
                        (fprintf p "inst props after setting the default %L\n" (getSGq (getSGq inst prop) name))
                        (setSGq par "2k" value)
                        (fprintf p "inst props after 2k %L types %L\n" (getSGq (getSGq inst prop) name)
                          (getSGq (getSGq inst prop) valueType))))
                    t)
            (fprintf p "instcdf ERROR\n"))
          (unless (errset
                    (progn
                      (dbReplaceProp cv "sfyProbeB" "boolean" t)
                      (fprintf p "boolean prop reads %L\n" (getSGq cv sfyProbeB))
                      (setq x (dbCreateArc cv (list "text" "drawing") (list (list 0.0 2.0) (list 1.0 3.0))
                                (list (list 0.0 2.0) (list 1.0 3.0))))
                      (when x
                        (fprintf p "arc created start=%L stop=%L\n" (getSGq x startAngle) (getSGq x stopAngle))
                        (setSGq x 0.5 startAngle)
                        (setSGq x 2.0 stopAngle)
                        (fprintf p "arc set start=%L stop=%L bBox=%L\n" (getSGq x startAngle)
                          (getSGq x stopAngle) (getSGq x bBox))))
                    t)
            (fprintf p "arcprop ERROR\n"))
          (unless (errset
                    (progn
                      (fprintf p "check %L\n" (schCheck cv))
                      (foreach q0 (getSGq cv prop)
                        (fprintf p "cvprop %L %L\n" (getSGq q0 name) (getSGq q0 valueType)))
                      (foreach f (getSGq cv shapes)
                        (fprintf p "shape %s %L parent=%L net=%L\n" (getSGq f objType) (getSGq f lpp)
                          (and (getSGq f parent) (getSGq (getSGq f parent) objType))
                          (and (getSGq f net) (getSGq (getSGq f net) name))))
                      (foreach i (getSGq cv instances)
                        (fprintf p "instance %L %L purpose=%L\n" (getSGq i name) (getSGq i cellName) (getSGq i purpose))))
                    t)
            (fprintf p "check ERROR\n")))
        (dbPurge cv)))
    (fprintf p "end\n")
    (close p)
    t))
