(prn (some? (System/getenv "PATH")))
(prn (System/getenv "CLJRSH_CONFORMANCE_NOPE"))
(prn (some? (System/getProperty "user.home")))
(prn (System/getProperty "nope.nope" "fallback"))
