(require '[babashka.cli :as cli])
(prn (cli/parse-opts ["--port" "8080" "--host" "x" "-v"] {:alias {:v :verbose}}))
(prn (cli/parse-opts ["--flag"] {:coerce {:flag :boolean}}))
(prn (:args (cli/parse-args ["cmd" "--n" "1" "rest"])))
