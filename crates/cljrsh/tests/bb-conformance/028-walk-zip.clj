(require '[clojure.walk :as walk])
(prn (walk/postwalk #(if (number? %) (inc %) %) {:a 1 :b [2 3]}))
(prn (walk/keywordize-keys {"a" 1 "b" {"c" 2}}))
(prn (walk/stringify-keys {:a 1}))
