//! Exercises the safe wrapper against a real on-disk environment.

use cljrs_lmdb::{DbiFlags, Env, EnvFlags};

fn open_env(dir: &std::path::Path) -> Env {
    Env::options()
        .map_size(64 << 20)
        .flags(EnvFlags::NO_TLS)
        .open(dir)
        .expect("environment must open")
}

#[test]
fn put_get_delete_roundtrip_and_reopen_persistence() {
    let dir = tempfile::tempdir().expect("tempdir");
    {
        let env = open_env(dir.path());
        let dbi = env.open_dbi("kv", DbiFlags::CREATE).expect("dbi");
        let mut txn = env.write_txn().expect("write txn");
        txn.put(dbi, b"alpha", b"1").expect("put");
        txn.put(dbi, b"beta", b"2").expect("put");
        txn.commit().expect("commit");

        let ro = env.read_txn().expect("read txn");
        assert_eq!(ro.get(dbi, b"alpha").expect("get"), Some(&b"1"[..]));
        assert_eq!(ro.get(dbi, b"missing").expect("get"), None);
        drop(ro);

        let mut txn = env.write_txn().expect("write txn");
        assert!(txn.del(dbi, b"alpha", None).expect("del"));
        assert!(!txn.del(dbi, b"alpha", None).expect("del again"));
        txn.commit().expect("commit");
    }
    let env = open_env(dir.path());
    let dbi = env.open_dbi("kv", DbiFlags::CREATE).expect("dbi");
    let ro = env.read_txn().expect("read txn");
    assert_eq!(ro.get(dbi, b"beta").expect("get"), Some(&b"2"[..]));
    assert_eq!(ro.get(dbi, b"alpha").expect("get"), None);
}

#[test]
fn range_scans_are_sorted_and_bounded_inclusively() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = open_env(dir.path());
    let dbi = env.open_dbi("sorted", DbiFlags::CREATE).expect("dbi");
    let mut txn = env.write_txn().expect("write txn");
    for key in ["b", "d", "a", "c", "e"] {
        txn.put(dbi, key.as_bytes(), b"x").expect("put");
    }
    txn.commit().expect("commit");

    let ro = env.read_txn().expect("read txn");
    let all: Vec<String> = ro
        .range(dbi, None, None)
        .expect("range")
        .map(|entry| String::from_utf8(entry.expect("entry").0.to_vec()).expect("utf8"))
        .collect();
    assert_eq!(all, ["a", "b", "c", "d", "e"]);

    let bounded: Vec<String> = ro
        .range(dbi, Some(b"b"), Some(b"d"))
        .expect("range")
        .map(|entry| String::from_utf8(entry.expect("entry").0.to_vec()).expect("utf8"))
        .collect();
    assert_eq!(bounded, ["b", "c", "d"]);
}

#[test]
fn dupsort_stores_sorted_duplicates_per_key() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = open_env(dir.path());
    let dbi = env
        .open_dbi("dups", DbiFlags::CREATE | DbiFlags::DUP_SORT)
        .expect("dbi");
    let mut txn = env.write_txn().expect("write txn");
    for value in ["v2", "v1", "v3"] {
        txn.put(dbi, b"k", value.as_bytes()).expect("put");
    }
    txn.put(dbi, b"other", b"solo").expect("put");
    txn.commit().expect("commit");

    let ro = env.read_txn().expect("read txn");
    let mut cursor = ro.cursor(dbi).expect("cursor");
    let (key, first) = cursor.set(b"k").expect("set").expect("found");
    assert_eq!((key, first), (&b"k"[..], &b"v1"[..]));
    assert_eq!(cursor.dup_count().expect("count"), 3);
    assert_eq!(
        cursor.next_dup().expect("next").map(|e| e.1),
        Some(&b"v2"[..])
    );
    assert_eq!(
        cursor.next_dup().expect("next").map(|e| e.1),
        Some(&b"v3"[..])
    );
    assert_eq!(cursor.next_dup().expect("next"), None);

    drop(cursor);
    drop(ro);
    let mut txn = env.write_txn().expect("write txn");
    assert!(txn.del(dbi, b"k", Some(b"v2")).expect("del pair"));
    txn.commit().expect("commit");
    let ro = env.read_txn().expect("read txn");
    let mut cursor = ro.cursor(dbi).expect("cursor");
    cursor.set(b"k").expect("set").expect("found");
    assert_eq!(cursor.dup_count().expect("count"), 2);
}

#[test]
fn counted_databases_answer_counts_and_ranks_without_scans() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = open_env(dir.path());
    let dbi = env
        .open_dbi("counted", DbiFlags::CREATE | DbiFlags::COUNTED)
        .expect("dbi");
    let mut txn = env.write_txn().expect("write txn");
    for i in 0..100_u32 {
        txn.put(dbi, format!("key{i:03}").as_bytes(), b"v")
            .expect("put");
    }
    txn.commit().expect("commit");

    let ro = env.read_txn().expect("read txn");
    assert_eq!(ro.count_all(dbi).expect("count_all"), 100);
    assert_eq!(
        ro.count_range(dbi, Some(b"key010"), Some(b"key019"))
            .expect("count_range"),
        10
    );
    assert_eq!(
        ro.count_range(dbi, None, Some(b"key004"))
            .expect("open low"),
        5
    );

    let (key, _) = ro.get_rank(dbi, 42).expect("get_rank").expect("present");
    assert_eq!(key, b"key042");
    assert_eq!(ro.get_rank(dbi, 100).expect("get_rank"), None);

    assert_eq!(ro.key_rank(dbi, b"key042").expect("key_rank"), Some(42));
    assert_eq!(ro.key_rank(dbi, b"nope").expect("key_rank"), None);
}

#[test]
fn prefix_compressed_counted_dbi_roundtrips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = open_env(dir.path());
    let dbi = env
        .open_dbi(
            "prefixed",
            DbiFlags::CREATE | DbiFlags::COUNTED | DbiFlags::PREFIX_COMPRESSION,
        )
        .expect("dbi");
    let mut txn = env.write_txn().expect("write txn");
    for i in 0..500_u32 {
        let key = format!("shared/long/common/prefix/{i:05}");
        txn.put(dbi, key.as_bytes(), &i.to_be_bytes()).expect("put");
    }
    txn.commit().expect("commit");

    let ro = env.read_txn().expect("read txn");
    assert_eq!(ro.count_all(dbi).expect("count"), 500);
    assert_eq!(
        ro.get(dbi, b"shared/long/common/prefix/00250")
            .expect("get"),
        Some(&250_u32.to_be_bytes()[..])
    );
}

#[test]
fn readers_see_a_stable_snapshot_while_a_writer_commits() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = open_env(dir.path());
    let dbi = env.open_dbi("snap", DbiFlags::CREATE).expect("dbi");
    let mut txn = env.write_txn().expect("write txn");
    txn.put(dbi, b"k", b"old").expect("put");
    txn.commit().expect("commit");

    let ro = env.read_txn().expect("read txn");
    let mut txn = env.write_txn().expect("write txn");
    txn.put(dbi, b"k", b"new").expect("put");
    txn.commit().expect("commit");

    assert_eq!(ro.get(dbi, b"k").expect("get"), Some(&b"old"[..]));
    let fresh = env.read_txn().expect("read txn");
    assert_eq!(fresh.get(dbi, b"k").expect("get"), Some(&b"new"[..]));
}

#[test]
fn abort_discards_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = open_env(dir.path());
    let dbi = env.open_dbi("abort", DbiFlags::CREATE).expect("dbi");
    let mut txn = env.write_txn().expect("write txn");
    txn.put(dbi, b"ghost", b"1").expect("put");
    txn.abort();
    let ro = env.read_txn().expect("read txn");
    assert_eq!(ro.get(dbi, b"ghost").expect("get"), None);
}

/// Child half of the cross-process test; run only via re-execution below.
#[test]
#[ignore = "child process helper for multiprocess_readers_share_the_environment"]
fn multiprocess_child_reader() {
    let path = std::env::var("CLJRS_LMDB_TEST_DIR").expect("child needs the env dir");
    let env = Env::options()
        .flags(EnvFlags::NO_TLS | EnvFlags::READ_ONLY)
        .open(std::path::Path::new(&path))
        .expect("child open");
    let dbi = env.open_dbi_read_only("shared").expect("child dbi");
    let ro = env.read_txn().expect("child read txn");
    assert_eq!(
        ro.get(dbi, b"greeting").expect("child get"),
        Some(&b"hello"[..])
    );
    assert_eq!(ro.count_all(dbi).expect("child count"), 1);
}

#[test]
fn multiprocess_readers_share_the_environment() {
    let dir = tempfile::tempdir().expect("tempdir");
    let env = open_env(dir.path());
    let dbi = env
        .open_dbi("shared", DbiFlags::CREATE | DbiFlags::COUNTED)
        .expect("dbi");
    let mut txn = env.write_txn().expect("write txn");
    txn.put(dbi, b"greeting", b"hello").expect("put");
    txn.commit().expect("commit");

    // Keep this process's environment open while a second process reads.
    let status = std::process::Command::new(std::env::current_exe().expect("self"))
        .args(["--ignored", "--exact", "multiprocess_child_reader"])
        .env("CLJRS_LMDB_TEST_DIR", dir.path())
        .status()
        .expect("child must run");
    assert!(status.success(), "child reader process failed");
}
