(require '[datalevin.join :as j]
         '[datalevin.relation :as r]
         '[datalevin.pipe :as p]
         '[datalevin.rules :as rules])
;; Hash join over shared attrs, incl. duplicate-key fan-out.
(def rel1 (r/relation! '{?e 0 ?n 1}
                       [(object-array [1 "Ivan"]) (object-array [2 "Petr"])
                        (object-array [3 "Oleg"])]))
(def rel2 (r/relation! '{?e 0 ?a 1}
                       [(object-array [1 20]) (object-array [2 30])
                        (object-array [2 31]) (object-array [4 50])]))
(def joined (j/hash-join rel1 rel2))
(prn (:attrs joined) (sort-by first (mapv vec (:tuples joined))))
;; Two shared attrs.
(def j2 (j/hash-join rel1 (r/relation! '{?n 0 ?a 1}
                                       [(object-array ["Ivan" 20])
                                        (object-array ["Petr" 99])])))
(prn (mapv vec (:tuples j2)))
(def dup (j/hash-join
           (r/relation! '{?e 0 ?t 1} [(object-array [1 :a])
                                      (object-array [1 :b])])
           (r/relation! '{?e 0 ?u 1} [(object-array [1 :c])
                                      (object-array [1 :d])])))
(prn (sort-by str (mapv vec (:tuples dup))))
;; Cartesian (no shared attrs), subtract, sink form.
(prn (mapv vec (:tuples (j/hash-join
                          (r/relation! '{?x 0} [(object-array [1])
                                                (object-array [2])])
                          (r/relation! '{?y 0} [(object-array [:a])])))))
(prn (mapv vec (:tuples (j/subtract-rel rel1 (r/relation! '{?e 0}
                                                          [(object-array [2])])))))
(let [sink (p/new-sink)]
  (j/hash-join-into rel1 rel2 sink)
  (prn (sort-by first (mapv vec (p/sink-seq sink)))))
;; Mutable collection shims: the idioms the vendored engine leans on.
(def fl (FastList.))
(.add fl 1)
(.addAll fl [2 3])
(prn (vec fl) (count fl) (.get fl 1) (into [] fl) (reduce + 0 fl))
(def hs (HashSet.))
(prn (.add hs :k) (.add hs :k) (.contains hs :k) (.size hs))
(def hm (HashMap.))
(.put hm :a 1)
(prn (.get hm :a) (.get hm :b) (.containsKey hm :a))
(prn (= (Object.) (Object.)))
(prn (let [v (volatile! 0)] (while (< @v 3) (vswap! v inc)) @v))
;; Rules: parsing, dependency analysis, stratification.
(def parsed (rules/parse-rules
  '[[(ancestor ?a ?b) [?a :parent ?b]]
    [(ancestor ?a ?b) [?a :parent ?c] (ancestor ?c ?b)]
    [(sibling ?a ?b) [?p :parent ?a] [?p :parent ?b]]]))
(prn (sort (keys parsed))
     (into {} (map (fn [[k v]] [k (count v)])) parsed))
(prn (sort-by str (mapv (comp sort vec)
                        (rules/tarjans-scc {:a [:b] :b [:c] :c [:a] :d [:a]}))))
(def dg (rules/dependency-graph parsed))
(prn dg)
(prn (mapv (comp sort vec) (rules/dependency-sccs dg)))
(prn (boolean (rules/recursive-stratum? #{'ancestor} dg 'ancestor))
     (boolean (rules/recursive-stratum? #{'sibling} dg 'sibling)))
