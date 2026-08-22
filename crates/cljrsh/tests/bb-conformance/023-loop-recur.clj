(prn (loop [i 0 acc []]
       (if (< i 5) (recur (inc i) (conj acc i)) acc)))
(defn fact [n] (loop [n n r 1] (if (zero? n) r (recur (dec n) (* r n)))))
(prn (fact 10))
(defn count-down [n] (if (zero? n) :done (recur (dec n))))
(prn (count-down 100000))
