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

;; Prints PASS or the first differing records; returns t on PASS. Files go to /tmp
;; (sfy_selftest_a.sfyx, sfy_selftest_b.sfyx and their .log). Also runs the retarget math
;; on its built-in vectors when sfy_retarget.il defines SfyRetargetSelfTest.
(procedure (SfySelfTest lib cell scratchLib)
  (let ((a "/tmp/sfy_selftest_a.sfyx") (b "/tmp/sfy_selftest_b.sfyx") (ok t) (msg nil))
    (printf "SfySelfTest %s/%s through %s\n" lib cell scratchLib)
    (unless (SfyExport a lib (list cell))
      (setq ok nil)
      (printf "  dump of %s/%s FAILED: see %s.log\n" lib cell a))
    (when ok
      (unless (SfyImport a ?lib scratchLib ?overwrite t ?check nil)
        (setq ok nil)
        (printf "  load into %s FAILED: see %s.log\n" scratchLib a)))
    (when ok
      (unless (SfyExport b scratchLib (list cell))
        (setq ok nil)
        (printf "  dump of %s/%s FAILED: see %s.log\n" scratchLib cell b)))
    (when ok
      (setq msg (SfyDiff (car (SfyReadRecords a)) (car (SfyReadRecords b)) lib scratchLib))
      (when msg
        (setq ok nil)
        (printf "  the dumps differ:\n  %s\n" msg)))
    (when (fboundp 'SfyRetargetSelfTest)
      (unless (SfyRetargetSelfTest)
        (setq ok nil)
        (printf "  retarget math FAILED\n")))
    (printf "SfySelfTest: %s\n" (if ok "PASS" "FAIL"))
    ok))
