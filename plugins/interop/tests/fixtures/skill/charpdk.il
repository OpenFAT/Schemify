;; charpdk.il: a made-up PDK library `zpdk` for the characterization tests (spec interop.md
;; 8.3). Its parameter names mean nothing on purpose: characterization must find inputs and
;; rules by experiment, never by name. Loaded after build.il (SfyFixSymbol, SfyFixCdf).
;; Numbers are written in exponent form so atof reads them.
;;
;; zeta  inputs zq (number), kappa (number), ct (count); derives rho and sig like a
;;       multi-finger area and perimeter over a table e(kappa) that rises and falls
;;       (50n below 14n, 44n below 16n, 48n below 20n, 60n above); kappa below 18n snaps to
;;       even nanometres; zq/ct is floored at 40n. flip (a boolean) and knob have callbacks
;;       that change nothing: not inputs. memo has no callback and is not netlisted.
;; tab   input mu (number, floored at 0.5u); nu is a table of mu (3 7 2 5, breakpoints at
;;       2u 5u 9u).
;; chaos inputs pp (number) and qq (count); zz is a hash of both: no rule explains it. pp
;;       set to an odd number of thousandths gains 0.1: no legality pattern explains it.
;; sel   inputs gx (number), mode (a choice of a b c) and dbl (a boolean); pz is 3gx, 5gx or
;;       0.7u by mode, qz is gx or 2gx by dbl: one rule per value of the selector. gx is
;;       floored at 1u, at 3u in mode c: its legality depends on the choice.

(procedure (zpdkGet n) (getSGq (cdfFindParamByName cdfgData n) value))
(procedure (zpdkSet n v) (setSGq (cdfFindParamByName cdfgData n) v value))

(procedure (zetaCB)
  (let ((zq (atof (zpdkGet "zq"))) (ka (atof (zpdkGet "kappa"))) (ct (zpdkGet "ct"))
        (e 0.0) (s 0) (d 0) (wf 0.0))
    (when (lessp ka 1.8e-08)
      (setq ka (times 2e-09 (round (quotient ka 2e-09))))
      (zpdkSet "kappa" (sprintf nil "%g" ka)))
    (when (lessp (quotient zq ct) 4e-08)
      (setq zq (times ct 4e-08))
      (zpdkSet "zq" (sprintf nil "%g" zq)))
    (setq e (cond ((lessp ka 1.4e-08) 5e-08) ((lessp ka 1.6e-08) 4.4e-08) ((lessp ka 2e-08) 4.8e-08) (t 6e-08)))
    (setq s (plus (quotient ct 2) 1))
    (setq d (quotient (plus ct 1) 2))
    (setq wf (quotient zq ct))
    (zpdkSet "rho" (sprintf nil "%g" (times s wf e)))
    (zpdkSet "sig" (sprintf nil "%g" (times d 2 (plus wf e))))
    t))

(procedure (tabCB)
  (let ((mu (atof (zpdkGet "mu"))) (nu 0))
    (when (lessp mu 5e-07)
      (setq mu 5e-07)
      (zpdkSet "mu" (sprintf nil "%g" mu)))
    (setq nu (cond ((lessp mu 2e-06) 3) ((lessp mu 5e-06) 7) ((lessp mu 9e-06) 2) (t 5)))
    (zpdkSet "nu" (sprintf nil "%d" nu))
    t))

(procedure (chaosCB)
  (let ((pp (atof (zpdkGet "pp"))) (qq (zpdkGet "qq")))
    (when (equal (mod (fix (times 1000 pp)) 2) 1)
      (setq pp (plus pp 0.1))
      (zpdkSet "pp" (sprintf nil "%g" pp)))
    (zpdkSet "zz" (sprintf nil "%d" (mod (plus (times 7 (fix (times 1000 pp))) (times 13 qq)) 11)))
    t))

(procedure (selCB)
  (let ((gx (atof (zpdkGet "gx"))) (mode (zpdkGet "mode")) (dbl (zpdkGet "dbl")) (f 1e-06))
    (when (equal mode "c") (setq f 3e-06))
    (when (lessp gx f)
      (setq gx f)
      (zpdkSet "gx" (sprintf nil "%g" gx)))
    (zpdkSet "pz" (sprintf nil "%g" (cond ((equal mode "a") (times 3 gx)) ((equal mode "b") (times 5 gx)) (t 7e-07))))
    (zpdkSet "qz" (sprintf nil "%g" (if dbl (times 2 gx) gx)))
    t))

(procedure (SfyFixCharPdk)
  (let ((cdf nil))
    (SfyFixSymbol "zpdk" "zeta" (list (list "A" "inputOutput" 0 40) (list "B" "inputOutput" 0 -40)))
    (setq cdf (SfyFixCdf "zpdk" "zeta"))
    (cdfCreateParam cdf ?name "zq" ?type "string" ?defValue "5e-08" ?prompt "zq" ?parseAsNumber "yes" ?callback "(zetaCB)")
    (cdfCreateParam cdf ?name "kappa" ?type "string" ?defValue "1e-08" ?prompt "kappa" ?parseAsNumber "yes" ?callback "(zetaCB)")
    (cdfCreateParam cdf ?name "ct" ?type "int" ?defValue 1 ?prompt "ct" ?callback "(zetaCB)")
    (cdfCreateParam cdf ?name "flip" ?type "boolean" ?prompt "flip" ?callback "(progn t)")
    (cdfCreateParam cdf ?name "knob" ?type "string" ?defValue "0" ?prompt "knob" ?callback "(progn t)")
    (cdfCreateParam cdf ?name "memo" ?type "string" ?defValue "hi" ?prompt "memo")
    (cdfCreateParam cdf ?name "rho" ?type "string" ?defValue "2.5e-15" ?prompt "rho" ?parseAsNumber "yes")
    (cdfCreateParam cdf ?name "sig" ?type "string" ?defValue "2e-07" ?prompt "sig" ?parseAsNumber "yes")
    (setSGq cdf (list nil 'spectre (list nil 'termOrder '(A B) 'instParameters '(zq kappa ct rho sig))) simInfo)
    (cdfSaveCDF cdf)
    (SfyFixSymbol "zpdk" "tab" (list (list "A" "inputOutput" 0 40) (list "B" "inputOutput" 0 -40)))
    (setq cdf (SfyFixCdf "zpdk" "tab"))
    (cdfCreateParam cdf ?name "mu" ?type "string" ?defValue "1e-06" ?prompt "mu" ?parseAsNumber "yes" ?callback "(tabCB)")
    (cdfCreateParam cdf ?name "nu" ?type "string" ?defValue "3" ?prompt "nu")
    (setSGq cdf (list nil 'spectre (list nil 'termOrder '(A B) 'instParameters '(mu nu))) simInfo)
    (cdfSaveCDF cdf)
    (SfyFixSymbol "zpdk" "chaos" (list (list "A" "inputOutput" 0 40) (list "B" "inputOutput" 0 -40)))
    (setq cdf (SfyFixCdf "zpdk" "chaos"))
    (cdfCreateParam cdf ?name "pp" ?type "string" ?defValue "1.5" ?prompt "pp" ?parseAsNumber "yes" ?callback "(chaosCB)")
    (cdfCreateParam cdf ?name "qq" ?type "int" ?defValue 1 ?prompt "qq" ?callback "(chaosCB)")
    (cdfCreateParam cdf ?name "zz" ?type "string" ?defValue "8" ?prompt "zz")
    (setSGq cdf (list nil 'spectre (list nil 'termOrder '(A B) 'instParameters '(pp qq zz))) simInfo)
    (cdfSaveCDF cdf)
    (SfyFixSymbol "zpdk" "sel" (list (list "A" "inputOutput" 0 40) (list "B" "inputOutput" 0 -40)))
    (setq cdf (SfyFixCdf "zpdk" "sel"))
    (cdfCreateParam cdf ?name "gx" ?type "string" ?defValue "2e-06" ?prompt "gx" ?parseAsNumber "yes" ?callback "(selCB)")
    (cdfCreateParam cdf ?name "mode" ?type "cyclic" ?choices (list "a" "b" "c") ?defValue "a" ?prompt "mode" ?callback "(selCB)")
    (cdfCreateParam cdf ?name "dbl" ?type "boolean" ?defValue nil ?prompt "dbl" ?callback "(selCB)")
    (cdfCreateParam cdf ?name "pz" ?type "string" ?defValue "6e-06" ?prompt "pz" ?parseAsNumber "yes")
    (cdfCreateParam cdf ?name "qz" ?type "string" ?defValue "2e-06" ?prompt "qz" ?parseAsNumber "yes")
    (setSGq cdf (list nil 'spectre (list nil 'termOrder '(A B) 'instParameters '(gx mode dbl pz qz))) simInfo)
    (cdfSaveCDF cdf)
    t))
