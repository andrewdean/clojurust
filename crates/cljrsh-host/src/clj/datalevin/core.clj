;;
;; cljrs replacement for the datalevin.core facade (upstream re-exports
;; the whole feature surface; this covers the Datalog subset the vendored
;; query family serves). Fn names, argument shapes, and semantics follow
;; upstream datalevin 78b199e8 core.clj (EPL-2.0, Copyright (c) Huahai
;; Yang). Transactions run through a minimal Datomic-style translator
;; over storage/load-datoms — [:db/add e a v], [:db/retract e a v], and
;; entity maps with tempid resolution; the full upstream tx pipeline
;; (uniqueness, upserts, components, lookup refs) is not ported yet.
;; Loaded on :cljrsh only.
;;
(ns datalevin.core
  (:require
   [datalevin.db :as db]
   [datalevin.datom :as d]
   [datalevin.pull-api :as dpa]
   [datalevin.query :as dq]
   [datalevin.query.predicate :as qpred]
   [datalevin.storage :as s]
   [datalevin.util :refer [raise]]))

;; ── query ────────────────────────────────────────────────────────────

(def q dq/q)
(def explain dq/explain)
(def pull dpa/pull)
(def pull-many dpa/pull-many)

(def forkable-predicate qpred/forkable-predicate)
(def forkable-predicate? qpred/forkable-predicate?)
(def fork-predicate qpred/fork-predicate)
(def fork-predicates qpred/fork-predicates)
(def shareable-predicate qpred/shareable-predicate)

;; ── db values ────────────────────────────────────────────────────────

(def empty-db db/empty-db)
(def init-db db/init-db)
(def close-db db/close-db)
(def db? db/-searchable?)

;; ── transactions ─────────────────────────────────────────────────────

(defn- tempid? [x]
  (or (and (integer? x) (neg? (long x)))
      (string? x)
      (keyword? x)))

(defn- resolve-eid
  "Resolve E to an entity id, allocating for tempids. Returns
  [eid tempids next-eid]."
  [e tempids next-eid]
  (cond
    (and (integer? e) (not (neg? (long e))))
    [(long e) tempids next-eid]

    (tempid? e)
    (if-some [known (get tempids e)]
      [known tempids next-eid]
      [next-eid (assoc tempids e next-eid) (inc (long next-eid))])

    :else
    (raise "Expected an entity id or tempid, got " e {:value e})))

(defn- entity-ops
  "[eid attr value add?] tuples for one map entity. Cardinality-many
  attrs accept collection values; nested entity maps are not supported
  (transact refs by id or tempid)."
  [d0 eid m]
  (into []
        (mapcat
          (fn [[a v]]
            (when-not (identical? a :db/id)
              (when (map? v)
                (raise "Nested entity maps are not supported; use explicit ids"
                       {:attr a :value v}))
              (if (and (db/multival? d0 a) (coll? v) (not (map? v)))
                (map (fn [v1] [eid a v1 true]) v)
                [[eid a v true]]))))
        m))

(defn- tx->datoms
  "Translate Datomic-style TX-DATA into datoms against D0. Supports
  [:db/add e a v], [:db/retract e a v], and entity maps; tempids
  (negative ints, strings, keywords) resolve in entity positions and
  in ref-attribute value positions. Returns {:datoms [..] :tempids {..}}."
  [d0 tx-data]
  (let [{:keys [ops tempids]}
        (loop [txs      (seq tx-data)
               tempids  {}
               next-eid (inc (long (or (db/max-eid d0) 0)))
               acc      []]
          (if txs
            (let [tx (first txs)]
              (cond
                (map? tx)
                (let [id (get tx :db/id)
                      [eid tempids next-eid]
                      (if (nil? id)
                        [next-eid tempids (inc (long next-eid))]
                        (resolve-eid id tempids next-eid))]
                  (recur (next txs) tempids (long next-eid)
                         (into acc (entity-ops d0 eid tx))))

                (sequential? tx)
                (let [[op e a v] tx
                      [eid tempids next-eid] (resolve-eid e tempids next-eid)]
                  (case op
                    :db/add
                    (recur (next txs) tempids (long next-eid)
                           (conj acc [eid a v true]))
                    :db/retract
                    (recur (next txs) tempids (long next-eid)
                           (conj acc [eid a v false]))
                    (raise "Unsupported tx op " op
                           {:op op :supported [:db/add :db/retract]})))

                :else
                (raise "Unsupported tx form " tx {:tx tx})))
            {:ops acc :tempids tempids}))
        resolve-v (fn [a v]
                    (if (and (db/ref? d0 a) (tempid? v))
                      (or (get tempids v)
                          (raise "Unresolved tempid in ref value " v
                                 {:attr a :value v}))
                      v))]
    {:datoms (mapv (fn [[e a v add?]]
                     (let [datom (d/datom e a (resolve-v a v))]
                       (if add? datom (d/delete datom))))
                   ops)
     :tempids tempids}))

(defn with
  "Apply Datomic-style TX-DATA to DB, returning a tx-report map
  {:db-before .. :db-after .. :tx-data .. :tempids ..}. The datoms are
  written to DB's store (the durable store has no speculative overlay)."
  [db tx-data]
  (let [{:keys [datoms tempids]} (tx->datoms db tx-data)
        store (:store db)]
    (s/load-datoms store datoms)
    {:db-before db
     :db-after  (db/new-db store)
     :tx-data   datoms
     :tempids   tempids}))

(defn db-with
  "DB with TX-DATA applied (written through to its store)."
  [db tx-data]
  (:db-after (with db tx-data)))

;; ── connections ──────────────────────────────────────────────────────

(defn get-conn
  "Open (creating if needed) a durable connection at DIR, with an
  optional datalevin schema map. Returns an atom holding the current
  db value."
  ([dir] (get-conn dir nil))
  ([dir schema] (get-conn dir schema nil))
  ([dir schema opts] (atom (empty-db dir schema opts))))

(def create-conn get-conn)

(defn conn-from-db [db] (atom db))

(defn conn? [x]
  (and (atom? x) (db? @x)))

(defn db [conn] @conn)

(defn transact!
  "Apply Datomic-style TX-DATA to a connection; returns the tx-report."
  [conn tx-data]
  (let [report (with @conn tx-data)]
    (reset! conn (:db-after report))
    report))

(defn close
  "Close a connection's underlying store."
  [conn]
  (s/close (:store @conn))
  nil)

;; ── index access ─────────────────────────────────────────────────────

(defn datoms
  ([db index] (db/-datoms db index))
  ([db index c1] (db/-datoms db index c1))
  ([db index c1 c2] (db/-datoms db index c1 c2))
  ([db index c1 c2 c3] (db/-datoms db index c1 c2 c3))
  ([db index c1 c2 c3 n] (db/-datoms db index c1 c2 c3 n)))

(defn seek-datoms
  ([db index] (db/-seek-datoms db index nil nil nil))
  ([db index c1] (db/-seek-datoms db index c1 nil nil))
  ([db index c1 c2] (db/-seek-datoms db index c1 c2 nil))
  ([db index c1 c2 c3] (db/-seek-datoms db index c1 c2 c3))
  ([db index c1 c2 c3 n] (db/-seek-datoms db index c1 c2 c3 n)))

(defn rseek-datoms
  ([db index] (db/-rseek-datoms db index nil nil nil))
  ([db index c1] (db/-rseek-datoms db index c1 nil nil))
  ([db index c1 c2] (db/-rseek-datoms db index c1 c2 nil))
  ([db index c1 c2 c3] (db/-rseek-datoms db index c1 c2 c3))
  ([db index c1 c2 c3 n] (db/-rseek-datoms db index c1 c2 c3 n)))

(defn search-datoms [db e a v] (db/-search db [e a v]))

(defn count-datoms [db e a v] (db/-count db [e a v]))

(defn cardinality [db attr] (db/-cardinality db attr))

(defn index-range [db attr start end] (db/-index-range db attr start end))

(defn entid [db eid] (db/entid db eid))

(defn max-eid [db] (db/max-eid db))

(defn schema [conn] (s/schema (:store @conn)))

(defn update-schema [conn schema]
  (doseq [[attr props] schema]
    (s/set-attr! (:store @conn) attr
                 {:cardinality-many (= :db.cardinality/many
                                       (:db/cardinality props))
                  :ref (= :db.type/ref (:db/valueType props))}))
  (reset! conn (db/new-db (:store @conn)))
  (schema conn))

(def datom d/datom)
(def datom-e d/datom-e)
(def datom-a d/datom-a)
(def datom-v d/datom-v)
(def datom? d/datom?)
