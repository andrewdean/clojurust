//! Order-preserving byte encoding for datom components.
//!
//! The store never installs a custom LMDB comparator: instead every value
//! encodes to bytes whose lexicographic order equals the value order. This
//! replaces datalevin's `bits.clj` (ByteBuffer + nippy) with a prefix-free
//! codec, so an encoded value can sit in the middle of a composite key
//! (`ave` keys are `[aid][value][eid]`) and still parse and sort correctly.
//!
//! Cross-type ordering is by tag byte, mirroring datalog's class-ordered
//! `value-compare`: values of different types never interleave.

/// One storable datom value.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreValue {
    Bool(bool),
    Long(i64),
    Double(f64),
    /// Epoch milliseconds; sorts with longs of its own tag.
    Instant(i64),
    Str(String),
    Keyword(String),
    Uuid([u8; 16]),
    /// An entity id, indexed in `vae` for reverse lookup.
    Ref(u64),
    Bytes(Vec<u8>),
    /// A heterogeneous tuple (datalevin's :db.type/tuple family); sorts
    /// element-wise, shorter prefixes first.
    Vec(Vec<StoreValue>),
}

pub const TAG_BOOL: u8 = 0x10;
pub const TAG_LONG: u8 = 0x20;
pub const TAG_DOUBLE: u8 = 0x30;
pub const TAG_INSTANT: u8 = 0x40;
pub const TAG_STR: u8 = 0x50;
pub const TAG_KEYWORD: u8 = 0x60;
pub const TAG_UUID: u8 = 0x70;
pub const TAG_REF: u8 = 0x80;
pub const TAG_BYTES: u8 = 0x90;
pub const TAG_VEC: u8 = 0xA0;
/// Vector terminator: below every element tag so `[1 2]` sorts before
/// `[1 2 3]`.
pub const TAG_VEC_END: u8 = 0x01;
/// Values whose encoding exceeds [`GIANT_THRESHOLD`]: the key holds the
/// tag, a fixed-length prefix of the inner encoding, and the giant id;
/// the full value lives in the giants DBI. Giants of one type sort after
/// that type's inline values, and order beyond the prefix follows
/// insertion id, not value order (documented divergence).
pub const TAG_GIANT_STR: u8 = 0x51;
pub const TAG_GIANT_BYTES: u8 = 0x91;

/// Inline-encoding size limit before a value overflows to the giants DBI.
pub const GIANT_THRESHOLD: usize = 400;
/// Raw prefix bytes kept in a giant's index key.
pub const GIANT_PREFIX: usize = 64;

/// Append `v`'s order-preserving encoding to `out`. Values that need the
/// giants DBI are NOT handled here; see `Store::encode_value_for_key`.
pub fn encode_inline(v: &StoreValue, out: &mut Vec<u8>) {
    match v {
        StoreValue::Bool(b) => {
            out.push(TAG_BOOL);
            out.push(u8::from(*b));
        }
        StoreValue::Long(n) => {
            out.push(TAG_LONG);
            out.extend_from_slice(&order_i64(*n));
        }
        StoreValue::Instant(n) => {
            out.push(TAG_INSTANT);
            out.extend_from_slice(&order_i64(*n));
        }
        StoreValue::Double(d) => {
            out.push(TAG_DOUBLE);
            out.extend_from_slice(&order_f64(*d));
        }
        StoreValue::Str(s) => {
            out.push(TAG_STR);
            push_escaped(s.as_bytes(), out);
        }
        StoreValue::Keyword(s) => {
            out.push(TAG_KEYWORD);
            push_escaped(s.as_bytes(), out);
        }
        StoreValue::Uuid(b) => {
            out.push(TAG_UUID);
            out.extend_from_slice(b);
        }
        StoreValue::Ref(e) => {
            out.push(TAG_REF);
            out.extend_from_slice(&e.to_be_bytes());
        }
        StoreValue::Bytes(b) => {
            out.push(TAG_BYTES);
            push_escaped(b, out);
        }
        StoreValue::Vec(items) => {
            out.push(TAG_VEC);
            for item in items {
                encode_inline(item, out);
            }
            out.push(TAG_VEC_END);
        }
    }
}

/// Decode one value from the front of `bytes`; returns the value and the
/// number of bytes consumed. Giant tags return the giant id instead of the
/// value; the caller resolves it against the giants DBI.
pub enum Decoded {
    Value(StoreValue),
    Giant { tag: u8, id: u64 },
}

/// # Errors
///
/// Returns a static description when the bytes do not parse.
pub fn decode(bytes: &[u8]) -> Result<(Decoded, usize), &'static str> {
    let (&tag, rest) = bytes.split_first().ok_or("empty value encoding")?;
    match tag {
        TAG_BOOL => {
            let b = *rest.first().ok_or("truncated bool")?;
            Ok((Decoded::Value(StoreValue::Bool(b != 0)), 2))
        }
        TAG_LONG | TAG_INSTANT => {
            let n = unorder_i64(rest.get(..8).ok_or("truncated long")?);
            let v = if tag == TAG_LONG {
                StoreValue::Long(n)
            } else {
                StoreValue::Instant(n)
            };
            Ok((Decoded::Value(v), 9))
        }
        TAG_DOUBLE => {
            let d = unorder_f64(rest.get(..8).ok_or("truncated double")?);
            Ok((Decoded::Value(StoreValue::Double(d)), 9))
        }
        TAG_STR | TAG_KEYWORD => {
            let (raw, used) = pop_escaped(rest)?;
            let s = String::from_utf8(raw).map_err(|_not_utf8| "invalid utf8")?;
            let v = if tag == TAG_STR {
                StoreValue::Str(s)
            } else {
                StoreValue::Keyword(s)
            };
            Ok((Decoded::Value(v), 1 + used))
        }
        TAG_UUID => {
            let mut b = [0_u8; 16];
            b.copy_from_slice(rest.get(..16).ok_or("truncated uuid")?);
            Ok((Decoded::Value(StoreValue::Uuid(b)), 17))
        }
        TAG_REF => {
            let mut b = [0_u8; 8];
            b.copy_from_slice(rest.get(..8).ok_or("truncated ref")?);
            Ok((Decoded::Value(StoreValue::Ref(u64::from_be_bytes(b))), 9))
        }
        TAG_BYTES => {
            let (raw, used) = pop_escaped(rest)?;
            Ok((Decoded::Value(StoreValue::Bytes(raw)), 1 + used))
        }
        TAG_VEC => {
            let mut items = Vec::new();
            let mut at = 1;
            loop {
                match bytes.get(at) {
                    None => return Err("unterminated vector encoding"),
                    Some(&TAG_VEC_END) => {
                        return Ok((Decoded::Value(StoreValue::Vec(items)), at + 1));
                    }
                    Some(_) => match decode(&bytes[at..])? {
                        (Decoded::Value(v), used) => {
                            items.push(v);
                            at += used;
                        }
                        (Decoded::Giant { .. }, _) => return Err("giant inside vector encoding"),
                    },
                }
            }
        }
        TAG_GIANT_STR | TAG_GIANT_BYTES => {
            // [tag][GIANT_PREFIX raw bytes][id:8]
            let id_at = GIANT_PREFIX;
            let idb = rest.get(id_at..id_at + 8).ok_or("truncated giant")?;
            let mut b = [0_u8; 8];
            b.copy_from_slice(idb);
            Ok((
                Decoded::Giant {
                    tag,
                    id: u64::from_be_bytes(b),
                },
                1 + GIANT_PREFIX + 8,
            ))
        }
        _ => Err("unknown value tag"),
    }
}

/// i64 → BE bytes whose unsigned order equals signed order.
fn order_i64(n: i64) -> [u8; 8] {
    ((n as u64) ^ (1 << 63)).to_be_bytes()
}

fn unorder_i64(bytes: &[u8]) -> i64 {
    let mut b = [0_u8; 8];
    b.copy_from_slice(bytes);
    (u64::from_be_bytes(b) ^ (1 << 63)) as i64
}

/// f64 → BE bytes in IEEE total order (NaN sorts above +inf).
fn order_f64(d: f64) -> [u8; 8] {
    let bits = d.to_bits();
    let ordered = if bits & (1 << 63) == 0 {
        bits | (1 << 63)
    } else {
        !bits
    };
    ordered.to_be_bytes()
}

fn unorder_f64(bytes: &[u8]) -> f64 {
    let mut b = [0_u8; 8];
    b.copy_from_slice(bytes);
    let ordered = u64::from_be_bytes(b);
    let bits = if ordered & (1 << 63) != 0 {
        ordered & !(1 << 63)
    } else {
        !ordered
    };
    f64::from_bits(bits)
}

/// Escape 0x00 → 0x00 0x01 and terminate with 0x00 0x00: prefix-free and
/// lexicographic order equals raw-bytes order.
fn push_escaped(raw: &[u8], out: &mut Vec<u8>) {
    for &b in raw {
        out.push(b);
        if b == 0 {
            out.push(1);
        }
    }
    out.push(0);
    out.push(0);
}

fn pop_escaped(bytes: &[u8]) -> Result<(Vec<u8>, usize), &'static str> {
    let mut raw = Vec::new();
    let mut i = 0;
    loop {
        let b = *bytes.get(i).ok_or("unterminated escaped bytes")?;
        i += 1;
        if b == 0 {
            let next = *bytes.get(i).ok_or("unterminated escape")?;
            i += 1;
            match next {
                0 => return Ok((raw, i)),
                1 => raw.push(0),
                _ => return Err("invalid escape"),
            }
        } else {
            raw.push(b);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    fn encode(v: &StoreValue) -> Vec<u8> {
        let mut out = Vec::new();
        encode_inline(v, out.as_mut());
        out
    }

    fn roundtrip(v: &StoreValue) {
        let enc = encode(v);
        // Append trailing noise: decode must stop at the value boundary.
        let mut padded = enc.clone();
        padded.extend_from_slice(b"trailing");
        let (decoded, used) = decode(&padded).expect("decode");
        assert_eq!(used, enc.len(), "prefix-free length for {v:?}");
        match decoded {
            Decoded::Value(got) => assert_eq!(&got, v),
            Decoded::Giant { .. } => panic!("inline value decoded as giant"),
        }
    }

    #[test]
    fn every_type_roundtrips_prefix_free() {
        for v in [
            StoreValue::Bool(false),
            StoreValue::Bool(true),
            StoreValue::Long(i64::MIN),
            StoreValue::Long(-1),
            StoreValue::Long(0),
            StoreValue::Long(i64::MAX),
            StoreValue::Double(-1.5),
            StoreValue::Double(0.0),
            StoreValue::Double(2.25),
            StoreValue::Instant(1_724_400_000_000),
            StoreValue::Str(String::new()),
            StoreValue::Str("hello".into()),
            StoreValue::Str("with\u{0}zero".into()),
            StoreValue::Keyword(":ns/name".into()),
            StoreValue::Uuid([7; 16]),
            StoreValue::Ref(42),
            StoreValue::Bytes(vec![0, 1, 0, 255]),
        ] {
            roundtrip(&v);
        }
    }

    /// Semantic order for same-type values (the codec's contract).
    fn semantic_cmp(a: &StoreValue, b: &StoreValue) -> Option<Ordering> {
        match (a, b) {
            (StoreValue::Long(x), StoreValue::Long(y)) => Some(x.cmp(y)),
            (StoreValue::Instant(x), StoreValue::Instant(y)) => Some(x.cmp(y)),
            (StoreValue::Double(x), StoreValue::Double(y)) => x.partial_cmp(y),
            (StoreValue::Str(x), StoreValue::Str(y)) => Some(x.as_bytes().cmp(y.as_bytes())),
            (StoreValue::Bytes(x), StoreValue::Bytes(y)) => Some(x.cmp(y)),
            (StoreValue::Ref(x), StoreValue::Ref(y)) => Some(x.cmp(y)),
            (StoreValue::Bool(x), StoreValue::Bool(y)) => Some(x.cmp(y)),
            _ => None,
        }
    }

    #[test]
    fn byte_order_equals_value_order_randomized() {
        // Deterministic xorshift so the corpus is stable run to run.
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut values = Vec::new();
        for _ in 0..400 {
            let r = next();
            let v = match r % 6 {
                0 => StoreValue::Long(next() as i64),
                1 => StoreValue::Double(f64::from_bits(next() & !(0x7FF << 52) | (1023 << 52))),
                2 => {
                    let len = (next() % 12) as usize;
                    let s: String = (0..len)
                        .map(|_| char::from(b'a' + (next() % 26) as u8))
                        .collect();
                    StoreValue::Str(s)
                }
                3 => {
                    let len = (next() % 8) as usize;
                    let b: Vec<u8> = (0..len).map(|_| (next() % 256) as u8).collect();
                    StoreValue::Bytes(b)
                }
                4 => StoreValue::Ref(next() % 1000),
                _ => StoreValue::Instant((next() % 1_000_000) as i64),
            };
            values.push(v);
        }
        for a in &values {
            for b in &values {
                if let Some(expected) = semantic_cmp(a, b) {
                    let got = encode(a).cmp(&encode(b));
                    assert_eq!(got, expected, "order mismatch: {a:?} vs {b:?}");
                }
            }
        }
    }

    #[test]
    fn negative_doubles_sort_below_positive_and_zero() {
        let order = [
            StoreValue::Double(f64::NEG_INFINITY),
            StoreValue::Double(-2.5),
            StoreValue::Double(-0.0),
            StoreValue::Double(0.0),
            StoreValue::Double(1.0e-9),
            StoreValue::Double(3.5),
            StoreValue::Double(f64::INFINITY),
        ];
        for pair in order.windows(2) {
            assert!(
                encode(&pair[0]) <= encode(&pair[1]),
                "{:?} must not sort above {:?}",
                pair[0],
                pair[1]
            );
        }
    }
}
