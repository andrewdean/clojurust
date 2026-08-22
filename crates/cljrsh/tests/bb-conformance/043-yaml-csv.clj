(require '[clj-yaml.core :as yaml] '[clojure.data.csv :as csv])
(prn (yaml/parse-string "a: 1\nlist:\n  - x\n  - y\n"))
(prn (csv/read-csv "h1,h2\nv1,\"v,2\"\n"))
(print (csv/write-csv-string [["a" "b"]]))
