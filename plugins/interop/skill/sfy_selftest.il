;; sfy_selftest.il: SfySelfTest, the one test a user runs in a real Virtuoso (interop spec
;; 5.2, 5.5): dump lib/cell, load the dump into scratchLib, dump that again, and compare the
;; two dumps record by record (library names aside).

;; t when x and y are equal, a string field reading lib in the first dump matching scratch
;; in the second (instances of the dump's own cells, the lib record).
(procedure (SfySame x y lib scratch)
  (cond
    ((equal x y) t)
    ((and (stringp x) (stringp y)) (and (equal x lib) (equal y scratch)))
    ((and (listp x) (listp y) (equal (length x) (length y)))
      (prog ()
        (while x
          (unless (SfySame (car x) (car y) lib scratch) (return nil))
          (setq x (cdr x))
          (setq y (cdr y)))
        (return t)))
    (t nil)))

;; nil when the record lists a and b match, else a message naming the first difference.
(procedure (SfyDiff a b lib scratch)
  (prog ()
    (while (and a b)
      (unless (SfySame (cadr (car a)) (cadr (car b)) lib scratch)
        (return (sprintf nil "first dump line %d: %L\n  second dump line %d: %L"
                  (car (car a)) (cadr (car a)) (car (car b)) (cadr (car b)))))
      (setq a (cdr a))
      (setq b (cdr b)))
    (return
      (cond
        (a (sprintf nil "the second dump ends before first dump line %d" (car (car a))))
        (b (sprintf nil "the second dump has more records from line %d" (car (car b))))
        (t nil)))))

;; Prints PASS or the first differing records and writes the same to
;; /tmp/sfy_selftest.txt; returns t on PASS. Files go to /tmp (sfy_selftest_a.sfyx,
;; sfy_selftest_b.sfyx and their .log). Also runs the retarget math on its built-in vectors
;; when sfy_retarget.il defines SfyRetargetSelfTest. Refuses a scratchLib that is lib (the
;; load overwrites) or is not in cds.lib, before touching anything.
(procedure (SfySelfTest lib cell scratchLib)
  (let ((a "/tmp/sfy_selftest_a.sfyx") (b "/tmp/sfy_selftest_b.sfyx") (ok t) (msg nil)
        (out nil) (r nil))
    (setq out (outfile "/tmp/sfy_selftest.txt" "w"))
    (setq msg
      (cond
        ((equal lib scratchLib)
          "scratchLib must not be lib: the load overwrites it (create an empty library)")
        ((null (ddGetObj scratchLib))
          (sprintf nil "library %s is not in cds.lib: create it in the Library Manager first"
            scratchLib))
        ((null (ddGetObj lib cell)) (sprintf nil "no cell %s/%s" lib cell))
        (t nil)))
    (SfySelfNote out (sprintf nil "SfySelfTest %s/%s through %s" lib cell scratchLib))
    (when msg
      (setq ok nil)
      (SfySelfNote out (strcat "  " msg)))
    (when ok
      (unless (SfyExport a lib (list cell))
        (setq ok nil)
        (SfySelfNote out (sprintf nil "  dump of %s/%s FAILED: see %s.log" lib cell a))))
    (when ok
      (unless (SfyImport a ?lib scratchLib ?overwrite t ?check nil)
        (setq ok nil)
        (SfySelfNote out (sprintf nil "  load into %s FAILED: see %s.log" scratchLib a))))
    (when ok
      (unless (SfyExport b scratchLib (list cell))
        (setq ok nil)
        (SfySelfNote out (sprintf nil "  dump of %s/%s FAILED: see %s.log" scratchLib cell b))))
    (when ok
      (setq msg (SfyDiff (SfySelfKeep (car (SfyReadRecords a))) (SfySelfKeep (car (SfyReadRecords b)))
                  lib scratchLib))
      (when msg
        (setq ok nil)
        (SfySelfNote out (sprintf nil "  the dumps differ:\n  %s" msg))))
    (when (fboundp 'SfyRetargetSelfTest)
      (setq r (errset (SfyRetargetSelfTest) t))
      (unless (and r (car r))
        (setq ok nil)
        (SfySelfNote out "  retarget math FAILED")))
    (SfySelfNote out (sprintf nil "SfySelfTest: %s" (if ok "PASS" "FAIL")))
    (when out (close out))
    ok))

;; The (line record) pairs of a dump without its schemify* properties: Schemify's ids and
;; stash, which Virtuoso may move to the other piece of a wire it splits and joins again
;; (live ICADVM20.1 run).
(procedure (SfySelfKeep recs)
  (setof r recs
    (not (and (equal (car (cadr r)) "prop")
              (stringp (nth 2 (cadr r)))
              (geqp (strlen (nth 2 (cadr r))) 8)
              (equal (substring (nth 2 (cadr r)) 1 8) "schemify")))))

;; One line to the CIW and to port out (when open), flushed.
(procedure (SfySelfNote out msg)
  (printf "%s\n" msg)
  (when out
    (fprintf out "%s\n" msg)
    (drain out)))
