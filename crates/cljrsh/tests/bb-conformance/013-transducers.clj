(prn (transduce (comp (map inc) (filter even?)) + 0 (range 10)))
(prn (into [] (comp (map inc) (take 3)) (range 10)))
(prn (sequence (map inc) [1 2 3]))
