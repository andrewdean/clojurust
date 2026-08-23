(ns ^:no-doc datascript.lru)

(declare assoc-lru cleanup-lru)

#?(:cljs
   (deftype LRU [key-value gen-key key-gen gen limit]
     IAssociative
     (-assoc [this k v] (assoc-lru this k v))
     (-contains-key? [_ k] (-contains-key? key-value k))
     ILookup
     (-lookup [_ k]    (-lookup key-value k nil))
     (-lookup [_ k nf] (-lookup key-value k nf))
     IPrintWithWriter
     (-pr-writer [_ writer opts]
       (-pr-writer key-value writer opts)))
   :cljrsh
   (deftype LRU [key-value gen-key key-gen gen limit])
   :clj
   (deftype LRU [^clojure.lang.Associative key-value gen-key key-gen gen limit]
     clojure.lang.ILookup
     (valAt [_ k]           (.valAt key-value k))
     (valAt [_ k not-found] (.valAt key-value k not-found))
     clojure.lang.Associative
     (containsKey [_ k] (.containsKey key-value k))
     (entryAt [_ k]     (.entryAt key-value k))
     (assoc [this k v]  (assoc-lru this k v))))

(defn assoc-lru [^LRU lru k v]
  (let [key-value (.-key-value lru)
        gen-key   (.-gen-key lru)
        key-gen   (.-key-gen lru)
        gen       (.-gen lru)
        limit     (.-limit lru)]
    (if-let [g (key-gen k nil)]
      (LRU.
        key-value
        (-> gen-key
          (dissoc g)
          (assoc gen k))
        (assoc key-gen k gen)
        (inc gen)
        limit)
      (cleanup-lru
        (LRU.
          (assoc key-value k v)
          (assoc gen-key gen k)
          (assoc key-gen k gen)
          (inc gen)
          limit)))))

(defn cleanup-lru [^LRU lru]
  (if (> (count (.-key-value lru)) (.-limit lru))
    (let [key-value (.-key-value lru)
          gen-key   (.-gen-key lru)
          key-gen   (.-key-gen lru)
          gen       (.-gen lru)
          limit     (.-limit lru)
          [g k]     (first gen-key)]
      (LRU.
        (dissoc key-value k)
        (dissoc gen-key g)
        (dissoc key-gen k)
        gen
        limit))
    lru))

(defn lru [limit]
  (LRU. {} (sorted-map) {} 0 limit))

(defprotocol ICache
  (-get [this key compute-fn]))

;; cljrsh: portable cache over plain maps (the LRU type above leans on
;; host Associative interfaces that only the JVM and JS hosts provide).
(defn cache [limit]
  (let [*impl (volatile! {:kv {} :gen-key (sorted-map) :key-gen {} :gen 0})]
    (reify ICache
      (-get [_ key compute-fn]
        (if-some [cached (get (:kv @*impl) key)]
          cached
          (let [computed (compute-fn)]
            (vswap! *impl
              (fn [{:keys [kv gen-key key-gen gen]}]
                (let [kv      (assoc kv key computed)
                      gen-key (assoc gen-key gen key)
                      key-gen (assoc key-gen key gen)]
                  (if (> (count kv) limit)
                    (let [[g k] (first gen-key)]
                      {:kv      (dissoc kv k)
                       :gen-key (dissoc gen-key g)
                       :key-gen (dissoc key-gen k)
                       :gen     (inc gen)})
                    {:kv kv :gen-key gen-key :key-gen key-gen :gen (inc gen)}))))
            computed))))))
