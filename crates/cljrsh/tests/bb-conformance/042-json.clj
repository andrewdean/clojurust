(require '[cheshire.core :as json])
(prn (json/parse-string "{\"a\":[1,2.5,null],\"b\":true}" true))
(println (json/generate-string {:name "x" :tags [:a]}))
(prn (json/parse-string (json/generate-string {:n 1}) true))
