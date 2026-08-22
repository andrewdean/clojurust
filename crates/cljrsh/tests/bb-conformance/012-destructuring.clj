(let [[a b & rest] [1 2 3 4 5]
      {:keys [x y] :or {y 10} :as m} {:x 1}
      [[p q] r] [[7 8] 9]]
  (prn [a b rest x y (:x m) p q r]))
(defn f [{:keys [name] :as opts} & args] [name (count args)])
(prn (f {:name "n"} 1 2 3))
