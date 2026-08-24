;;
;; cljrs replacement for datalevin.validate (upstream is 1.7k lines,
;; mostly storage/serde checks the Rust store owns). These are the
;; validators the query family uses; messages and error data follow
;; upstream datalevin 78b199e8 validate.clj (EPL-2.0, Copyright (c)
;; Huahai Yang). Loaded on :cljrsh only.
;;
(ns ^:no-doc datalevin.validate
  "Validation functions used by the Datalog query family"
  (:require
   [datalevin.util :as u]))

(defn validate-attr
  "Validate that an attribute is a keyword."
  [attr at]
  (when-not (keyword? attr)
    (u/raise "Bad entity attribute " attr " at " at ", expected keyword"
             {:error :transact/syntax, :attribute attr, :context at})))

(defn validate-val
  "Validate that a value is not nil."
  [v at]
  (when (nil? v)
    (u/raise "Cannot store nil as a value at " at
             {:error :transact/syntax, :value v, :context at})))

(defn validate-lookup-ref-shape
  "Validate that a lookup ref contains exactly 2 elements."
  [eid]
  (when (not= (count eid) 2)
    (u/raise "Lookup ref should contain 2 elements: " eid
             {:error :lookup-ref/syntax, :entity-id eid})))

(defn validate-lookup-ref-unique
  "Validate that a lookup ref attribute is marked as :db/unique."
  [unique? eid]
  (when-not unique?
    (u/raise "Lookup ref attribute should be marked as :db/unique: " eid
             {:error :lookup-ref/unique, :entity-id eid})))

(defn validate-entity-id-syntax
  "Validate entity id syntax: must be a number or lookup ref."
  [eid]
  (u/raise "Expected number or lookup ref for entity id, got " eid
           {:error :entity-id/syntax, :entity-id eid}))

(defn validate-map-entity-id-syntax
  "Validate :db/id in a map entity: must be a number, string, or lookup ref."
  [eid]
  (u/raise "Expected number, string or lookup ref for :db/id, got " eid
           {:error :entity-id/syntax, :entity-id eid}))

(defn validate-entity-id-exists
  "Validate that an entity id resolves to an existing entity."
  [result eid]
  (when-not result
    (u/raise "Nothing found for entity id " eid
             {:error     :entity-id/missing
              :entity-id eid})))

(defn validate-reverse-ref-attr
  "Validate that a reverse-ref attribute is a keyword."
  [attr]
  (when-not (keyword? attr)
    (u/raise "Bad entity attribute: " attr ", expected keyword"
             {:error :transact/syntax, :attribute attr})))
