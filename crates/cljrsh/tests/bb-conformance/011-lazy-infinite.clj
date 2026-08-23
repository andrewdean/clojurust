(prn (take 5 (iterate (partial * 2) 1)))
(prn (take 4 (cycle [:a :b])))
(prn (take 3 (repeatedly (constantly :x))))
(prn (first (filter #(> % 100) (map #(* % %) (range)))))
