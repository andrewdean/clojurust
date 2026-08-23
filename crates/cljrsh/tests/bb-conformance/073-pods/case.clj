(require '[babashka.pods :as pods])
(pods/load-pod (System/getenv "CLJRSH_TEST_POD"))
(require '[pod.test-pod :as tp])
(println (tp/add-sync 20 22))
