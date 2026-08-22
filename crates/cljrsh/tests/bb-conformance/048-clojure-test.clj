(require '[clojure.test :as t :refer [deftest is testing]])
(deftest arithmetic
  (testing "adds" (is (= 4 (+ 2 2))))
  (is (= 6 (* 2 3))))
(def result (t/run-tests))
(prn [(:pass result) (:fail result) (:error result)])
