(require '[datalevin.built-ins :as bi])
;; SQL LIKE semantics: % = any run, _ = one char, escape char quotes the next.
(prn (bi/like "Smith" "%mit%")
     (bi/like "Smith" "S_ith")
     (bi/like "Smith" "J%"))
(prn (bi/like "50%" "50!%" {:escape \!})
     (bi/like "50x" "50!%" {:escape \!}))
(prn (bi/not-like "Smith" "J%")
     (bi/not-like "Smith" "%mit%"))
(prn (bi/in "a" ["a" "b"])
     (bi/not-in "c" ["a" "b"]))
;; query-fns is the registry the optimizer resolves predicate/fn symbols in.
(prn (count (keys bi/query-fns)))
(prn ((get bi/query-fns '<) 1 2 3)
     ((get bi/query-fns 'max) 3 9 2)
     ((get bi/query-fns 'and) 1 2)
     ((get bi/query-fns 'or) nil 5)
     ((get bi/query-fns 'ground) :x))
;; Storage-dependent built-ins stub out until the db/storage adapters land.
(prn (try (bi/fulltext :db "q") (catch Exception _ :raised)))
;; Regression: qualified special forms ((clojure.core/or ...) inside the
;; vendored like body) must dispatch as special forms, not stub calls.
(prn (clojure.core/or nil :x) (clojure.core/and 1 2))
