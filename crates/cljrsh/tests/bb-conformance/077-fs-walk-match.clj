(require '[babashka.fs :as fs])
(def d (fs/create-temp-dir))
(fs/create-dirs (fs/path d "x" "y"))
(spit (fs/path d "a.clj") "1")
(spit (fs/path d "x" "b.clj") "2")
(spit (fs/path d "x" "y" "c.txt") "3")
(prn (mapv fs/file-name (sort (fs/match d "glob:*.clj"))))
(prn (mapv fs/file-name (sort (fs/match d "glob:**.clj" {:recursive true}))))
(prn (mapv fs/file-name (sort (fs/match d "regex:.*\\.txt" {:recursive true}))))
(def events (atom []))
(fs/walk-file-tree d
  {:pre-visit-dir (fn [p _]
                    (swap! events conj (str "pre [" (fs/relativize d p) "]"))
                    (if (= "y" (fs/file-name p)) :skip-subtree :continue))
   :post-visit-dir (fn [p _]
                     (swap! events conj (str "post [" (fs/relativize d p) "]"))
                     :continue)
   :visit-file (fn [p _]
                 (swap! events conj (str "file " (fs/file-name p)))
                 :continue)})
(run! println @events)
(fs/delete-tree d)
