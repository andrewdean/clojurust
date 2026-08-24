//! Exercises the triple store end to end on disk.

use cljrs_datalog_store::{AttrProps, Datom, Op, Store, StoreValue};

fn add(e: u64, a: &str, v: StoreValue) -> Op {
    Op::Add { e, a: a.into(), v }
}

fn retract(e: u64, a: &str, v: StoreValue) -> Op {
    Op::Retract { e, a: a.into(), v }
}

fn s(v: &str) -> StoreValue {
    StoreValue::Str(v.into())
}

#[test]
fn search_covers_the_pattern_case_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    store
        .set_attr(
            "friend",
            AttrProps {
                cardinality_many: true,
                ref_type: false,
            },
        )
        .expect("schema");
    store
        .transact(&[
            add(1, "name", s("Ivan")),
            add(1, "age", StoreValue::Long(39)),
            add(2, "name", s("Petr")),
            add(2, "age", StoreValue::Long(22)),
            add(2, "friend", StoreValue::Ref(1)),
            add(2, "friend", StoreValue::Ref(3)),
            add(3, "name", s("Oleg")),
        ])
        .expect("transact");

    // (e a v) exact
    assert_eq!(
        store
            .search(Some(1), Some("name"), Some(&s("Ivan")))
            .expect("eav")
            .len(),
        1
    );
    assert!(
        store
            .search(Some(1), Some("name"), Some(&s("Nope")))
            .expect("eav")
            .is_empty()
    );
    // (e a _)
    let names: Vec<StoreValue> = store
        .search(Some(2), Some("friend"), None)
        .expect("ea")
        .into_iter()
        .map(|d| d.v)
        .collect();
    assert_eq!(names, [StoreValue::Ref(1), StoreValue::Ref(3)]);
    // (e _ _)
    assert_eq!(store.search(Some(2), None, None).expect("e").len(), 4);
    // (_ a v)
    let hits = store
        .search(None, Some("age"), Some(&StoreValue::Long(22)))
        .expect("av");
    assert_eq!(
        hits,
        [Datom {
            e: 2,
            a: "age".into(),
            v: StoreValue::Long(22)
        }]
    );
    // (_ a _) sorted by value: 22 before 39
    let ages: Vec<u64> = store
        .search(None, Some("age"), None)
        .expect("a")
        .into_iter()
        .map(|d| d.e)
        .collect();
    assert_eq!(ages, [2, 1]);
    // (_ _ ref-v) via vae
    let back = store
        .search(None, None, Some(&StoreValue::Ref(1)))
        .expect("vae");
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].e, 2);
    assert_eq!(back[0].a, "friend");
    // (_ _ _)
    assert_eq!(store.search(None, None, None).expect("all").len(), 7);
    // unknown attribute matches nothing
    assert!(
        store
            .search(None, Some("ghost"), None)
            .expect("none")
            .is_empty()
    );
}

#[test]
fn cardinality_one_replaces_and_many_accumulates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    store
        .set_attr(
            "alias",
            AttrProps {
                cardinality_many: true,
                ref_type: false,
            },
        )
        .expect("schema");

    store.transact(&[add(1, "name", s("Ivan"))]).expect("tx");
    store.transact(&[add(1, "name", s("Ivan II"))]).expect("tx");
    let names = store.search(Some(1), Some("name"), None).expect("names");
    assert_eq!(names.len(), 1, "cardinality-one must replace");
    assert_eq!(names[0].v, s("Ivan II"));
    // The old value is gone from ave too.
    assert!(
        store
            .search(None, Some("name"), Some(&s("Ivan")))
            .expect("ave")
            .is_empty()
    );

    store
        .transact(&[add(1, "alias", s("vanya")), add(1, "alias", s("ivanushka"))])
        .expect("tx");
    assert_eq!(
        store
            .search(Some(1), Some("alias"), None)
            .expect("aliases")
            .len(),
        2
    );
}

#[test]
fn retract_removes_from_every_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    store
        .transact(&[add(1, "boss", StoreValue::Ref(2)), add(1, "name", s("x"))])
        .expect("tx");
    assert_eq!(
        store
            .search(None, None, Some(&StoreValue::Ref(2)))
            .expect("vae")
            .len(),
        1
    );
    store
        .transact(&[retract(1, "boss", StoreValue::Ref(2))])
        .expect("tx");
    assert!(
        store
            .search(Some(1), Some("boss"), None)
            .expect("eav")
            .is_empty()
    );
    assert!(
        store
            .search(None, Some("boss"), None)
            .expect("ave")
            .is_empty()
    );
    assert!(
        store
            .search(None, None, Some(&StoreValue::Ref(2)))
            .expect("vae")
            .is_empty()
    );
    // Retracting an absent datom is a no-op.
    store
        .transact(&[retract(1, "boss", StoreValue::Ref(2))])
        .expect("tx");
}

#[test]
fn counts_and_samples_come_from_the_counted_indexes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    let ops: Vec<Op> = (0..200_u64)
        .map(|i| add(i + 1, "score", StoreValue::Long((i % 50) as i64)))
        .collect();
    store.transact(&ops).expect("tx");

    assert_eq!(store.count(None, None, None).expect("all"), 200);
    assert_eq!(store.count(None, Some("score"), None).expect("attr"), 200);
    assert_eq!(
        store
            .count(None, Some("score"), Some(&StoreValue::Long(7)))
            .expect("av"),
        4
    );
    assert_eq!(store.count(Some(5), None, None).expect("e"), 1);
    assert_eq!(store.count(Some(5), Some("score"), None).expect("ea"), 1);
    assert_eq!(store.cardinality("score").expect("card"), 200);

    let sample = store.sample_ave("score", 10).expect("sample");
    assert_eq!(sample.len(), 10);
    // Samples arrive in value order and span the range.
    let values: Vec<i64> = sample
        .iter()
        .map(|d| match d.v {
            StoreValue::Long(n) => n,
            _ => panic!("unexpected type"),
        })
        .collect();
    let mut sorted = values.clone();
    sorted.sort_unstable();
    assert_eq!(values, sorted);
    assert!(values.first().expect("first") < values.last().expect("last"));

    assert_eq!(store.max_eid().expect("max"), 200);
}

#[test]
fn giant_values_roundtrip_and_match_exactly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    let big_a = "a".repeat(5000) + "-suffix-one";
    let big_b = "a".repeat(5000) + "-suffix-two";
    store
        .transact(&[add(1, "doc", s(&big_a)), add(2, "doc", s(&big_b))])
        .expect("tx");

    let hit = store
        .search(None, Some("doc"), Some(&s(&big_a)))
        .expect("giant av");
    assert_eq!(hit.len(), 1);
    assert_eq!(hit[0].e, 1);
    assert_eq!(hit[0].v, s(&big_a));

    // A giant value never stored matches nothing (and allocates nothing).
    let never = "z".repeat(5000);
    assert!(
        store
            .search(None, Some("doc"), Some(&s(&never)))
            .expect("miss")
            .is_empty()
    );

    // Both giants decode on full scans.
    let all = store.search(None, Some("doc"), None).expect("scan");
    assert_eq!(all.len(), 2);
    assert!(all.iter().any(|d| d.v == s(&big_b)));
}

#[test]
fn store_persists_across_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let store = Store::open(dir.path()).expect("open");
        store
            .set_attr(
                "tag",
                AttrProps {
                    cardinality_many: true,
                    ref_type: false,
                },
            )
            .expect("schema");
        store
            .transact(&[add(7, "tag", s("alpha")), add(7, "tag", s("beta"))])
            .expect("tx");
    }
    let store = Store::open(dir.path()).expect("reopen");
    assert_eq!(
        store.attr_props("tag"),
        Some(AttrProps {
            cardinality_many: true,
            ref_type: false,
        }),
        "schema must survive reopen"
    );
    assert_eq!(
        store.search(Some(7), Some("tag"), None).expect("eav").len(),
        2
    );
    assert_eq!(store.max_eid().expect("max"), 7);
    // New assertions keep accumulating under the reloaded schema.
    store.transact(&[add(7, "tag", s("gamma"))]).expect("tx");
    assert_eq!(
        store.search(Some(7), Some("tag"), None).expect("eav").len(),
        3
    );
}

#[test]
fn mixed_value_types_sort_within_their_type() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    store
        .transact(&[
            add(1, "v", StoreValue::Long(-5)),
            add(2, "v", StoreValue::Long(3)),
            add(3, "v", StoreValue::Double(-0.5)),
            add(4, "v", StoreValue::Double(2.5)),
            add(5, "v", s("apple")),
            add(6, "v", s("banana")),
        ])
        .expect("tx");
    let order: Vec<u64> = store
        .search(None, Some("v"), None)
        .expect("scan")
        .into_iter()
        .map(|d| d.e)
        .collect();
    // Longs (-5 < 3), then doubles (-0.5 < 2.5), then strings.
    assert_eq!(order, [1, 2, 3, 4, 5, 6]);
}

#[test]
fn ref_typed_attrs_and_next_eid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open");
    store
        .set_attr(
            "boss",
            AttrProps {
                cardinality_many: false,
                ref_type: true,
            },
        )
        .expect("schema");
    store
        .transact(&[add(3, "boss", StoreValue::Ref(9)), add(7, "name", s("x"))])
        .expect("tx");
    assert!(store.attr_props("boss").expect("props").ref_type);
    let listed = store.attrs();
    assert!(listed.iter().any(|(n, p)| n == "boss" && p.ref_type));
    assert_eq!(store.next_eid(0).expect("next"), Some(3));
    assert_eq!(store.next_eid(3).expect("next"), Some(3));
    assert_eq!(store.next_eid(4).expect("next"), Some(7));
    assert_eq!(store.next_eid(8).expect("next"), None);
}
