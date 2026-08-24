//! Datalog triple store over [`cljrs_lmdb`] (datalog-plan.md phase 3).
//!
//! Datoms live in three counted indexes whose keys are order-preserving
//! byte encodings (see [`codec`]):
//!
//! - `eav`: `[e:8][aid:4][value]` — entity-first scans
//! - `ave`: `[aid:4][value][e:8]` — attribute/value scans, prefix-compressed
//! - `vae`: `[v:8][aid:4][e:8]` — reverse lookup for ref values
//!
//! Attributes are interned to 4-byte aids in the `schema` DBI; values whose
//! encoding exceeds the giant threshold overflow to the content-addressed
//! `giants` DBI. Counts and ranks come from dlmdb's counted databases, so
//! optimizer statistics (`count`, `cardinality`, `sample_ave`) are O(log n)
//! per probe rather than scans.
//!
//! This is the storage half beneath the ported Clojure query engine
//! (phase 4). Upsert resolution, tempids, and transaction reports stay
//! above; the store speaks raw adds and retracts of resolved datoms.

pub mod codec;

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use cljrs_lmdb::{Dbi, DbiFlags, Env, EnvFlags, RoTxn, RwTxn};
use sha2::{Digest, Sha256};

pub use codec::StoreValue;

/// One resolved datom.
#[derive(Debug, Clone, PartialEq)]
pub struct Datom {
    pub e: u64,
    pub a: String,
    pub v: StoreValue,
}

/// One transaction operation over resolved datoms.
#[derive(Debug, Clone)]
pub enum Op {
    Add { e: u64, a: String, v: StoreValue },
    Retract { e: u64, a: String, v: StoreValue },
}

/// Attribute properties the store enforces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttrProps {
    /// Cardinality-many keeps every asserted value; cardinality-one (the
    /// default) replaces the previous value on assert.
    pub cardinality_many: bool,
    /// Ref-typed attributes index their entity-id values in `vae`; the
    /// host layer coerces integer values to [`StoreValue::Ref`] for them.
    pub ref_type: bool,
}

#[derive(Debug)]
pub enum StoreError {
    Lmdb(cljrs_lmdb::Error),
    Codec(&'static str),
    /// A giant id in an index key had no giants entry.
    MissingGiant(u64),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::Lmdb(e) => write!(f, "{e}"),
            StoreError::Codec(m) => write!(f, "codec error: {m}"),
            StoreError::MissingGiant(id) => write!(f, "missing giant value {id}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<cljrs_lmdb::Error> for StoreError {
    fn from(e: cljrs_lmdb::Error) -> Self {
        StoreError::Lmdb(e)
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

const META_MAX_EID: &[u8] = b"max-eid";
const META_NEXT_AID: &[u8] = b"next-aid";
const META_NEXT_GIANT: &[u8] = b"next-giant";

const FLAG_CARD_MANY: u8 = 0x01;
const FLAG_REF: u8 = 0x02;

/// The triple store.
pub struct Store {
    env: Env,
    eav: Dbi,
    ave: Dbi,
    vae: Dbi,
    schema: Dbi,
    meta: Dbi,
    giants: Dbi,
    giant_ids: Dbi,
    /// attr name → (aid, props); loaded at open, extended on intern.
    attrs: RwLock<HashMap<String, (u32, AttrProps)>>,
    /// aid → attr name for decoding index keys.
    attr_names: RwLock<HashMap<u32, String>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").finish_non_exhaustive()
    }
}

impl Store {
    /// Open (creating if needed) a store in the directory at `path`.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage error.
    pub fn open(path: &Path) -> Result<Store> {
        Store::open_with_flags(path, EnvFlags::NO_TLS)
    }

    /// Open with extra environment flags ([`EnvFlags::IN_MEMORY`] for the
    /// dlmdb in-memory mode).
    ///
    /// # Errors
    ///
    /// Returns the underlying storage error.
    pub fn open_with_flags(path: &Path, flags: EnvFlags) -> Result<Store> {
        // LMDB requires the environment directory to exist.
        std::fs::create_dir_all(path)
            .map_err(|_io| StoreError::Codec("cannot create store dir"))?;
        let env = Env::options()
            .map_size(1 << 30)
            .max_dbs(16)
            .flags(flags | EnvFlags::NO_TLS)
            .open(path)?;
        let eav = env.open_dbi("eav", DbiFlags::CREATE | DbiFlags::COUNTED)?;
        let ave = env.open_dbi(
            "ave",
            DbiFlags::CREATE | DbiFlags::COUNTED | DbiFlags::PREFIX_COMPRESSION,
        )?;
        let vae = env.open_dbi("vae", DbiFlags::CREATE | DbiFlags::COUNTED)?;
        let schema = env.open_dbi("schema", DbiFlags::CREATE)?;
        let meta = env.open_dbi("meta", DbiFlags::CREATE)?;
        let giants = env.open_dbi("giants", DbiFlags::CREATE)?;
        let giant_ids = env.open_dbi("giant-ids", DbiFlags::CREATE)?;

        let mut attrs = HashMap::new();
        let mut attr_names = HashMap::new();
        {
            let ro = env.read_txn()?;
            for entry in ro.range(schema, None, None)? {
                let (k, v) = entry?;
                let name = String::from_utf8(k.to_vec())
                    .map_err(|_not_utf8| StoreError::Codec("schema attr not utf8"))?;
                let aid = u32::from_be_bytes(
                    v.get(..4)
                        .ok_or(StoreError::Codec("short schema entry"))?
                        .try_into()
                        .expect("length checked"),
                );
                let flags = *v.get(4).ok_or(StoreError::Codec("short schema entry"))?;
                let props = AttrProps {
                    cardinality_many: flags & FLAG_CARD_MANY != 0,
                    ref_type: flags & FLAG_REF != 0,
                };
                attrs.insert(name.clone(), (aid, props));
                attr_names.insert(aid, name);
            }
        }
        Ok(Store {
            env,
            eav,
            ave,
            vae,
            schema,
            meta,
            giants,
            giant_ids,
            attrs: RwLock::new(attrs),
            attr_names: RwLock::new(attr_names),
        })
    }

    /// Declare or update an attribute's properties before use.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage error.
    pub fn set_attr(&self, name: &str, props: AttrProps) -> Result<()> {
        let mut txn = self.env.write_txn()?;
        self.intern_attr(&mut txn, name, Some(props))?;
        txn.commit()?;
        Ok(())
    }

    /// The properties an attribute currently carries.
    #[must_use]
    pub fn attr_props(&self, name: &str) -> Option<AttrProps> {
        self.attrs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(|(_aid, props)| *props)
    }

    fn intern_attr(
        &self,
        txn: &mut RwTxn<'_>,
        name: &str,
        props: Option<AttrProps>,
    ) -> Result<(u32, AttrProps)> {
        let existing = self
            .attrs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .copied();
        let (aid, props) = match (existing, props) {
            (Some((aid, current)), None) => return Ok((aid, current)),
            (Some((aid, _)), Some(p)) => (aid, p),
            (None, p) => {
                let aid = self.bump_counter(txn, META_NEXT_AID)? as u32;
                (aid, p.unwrap_or_default())
            }
        };
        let mut rec = aid.to_be_bytes().to_vec();
        let mut flag_byte = 0;
        if props.cardinality_many {
            flag_byte |= FLAG_CARD_MANY;
        }
        if props.ref_type {
            flag_byte |= FLAG_REF;
        }
        rec.push(flag_byte);
        txn.put(self.schema, name.as_bytes(), &rec)?;
        self.attrs
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_owned(), (aid, props));
        self.attr_names
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(aid, name.to_owned());
        Ok((aid, props))
    }

    fn bump_counter(&self, txn: &mut RwTxn<'_>, key: &[u8]) -> Result<u64> {
        let current = txn
            .get(self.meta, key)?
            .map(|b| {
                b.try_into()
                    .map(u64::from_be_bytes)
                    .map_err(|_bad_len| StoreError::Codec("bad meta counter"))
            })
            .transpose()?
            .unwrap_or(0);
        let next = current + 1;
        txn.put(self.meta, key, &next.to_be_bytes())?;
        Ok(next)
    }

    /// Every declared attribute with its properties.
    #[must_use]
    pub fn attrs(&self) -> Vec<(String, AttrProps)> {
        let mut out: Vec<(String, AttrProps)> = self
            .attrs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(name, (_aid, props))| (name.clone(), *props))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The smallest entity id >= `from` that has any datom, or None.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage error.
    pub fn next_eid(&self, from: u64) -> Result<Option<u64>> {
        let ro = self.env.read_txn()?;
        let mut cursor = ro.cursor(self.eav)?;
        match cursor.set_range(&from.to_be_bytes())? {
            None => Ok(None),
            Some((k, _)) => Ok(Some(u64::from_be_bytes(
                k.get(..8)
                    .ok_or(StoreError::Codec("short eav key"))?
                    .try_into()
                    .expect("length checked"),
            ))),
        }
    }

    /// The largest entity id ever asserted.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage error.
    pub fn max_eid(&self) -> Result<u64> {
        let ro = self.env.read_txn()?;
        Ok(ro
            .get(self.meta, META_MAX_EID)?
            .and_then(|b| b.try_into().ok().map(u64::from_be_bytes))
            .unwrap_or(0))
    }

    /// Encode `v` for use inside an index key, spilling oversized values
    /// to the giants DBI (content-addressed, so equal values share an id).
    fn encode_value_for_key(&self, txn: &mut RwTxn<'_>, v: &StoreValue) -> Result<Vec<u8>> {
        let mut inline = Vec::new();
        codec::encode_inline(v, &mut inline);
        if inline.len() <= codec::GIANT_THRESHOLD {
            return Ok(inline);
        }
        let (giant_tag, raw): (u8, &[u8]) = match v {
            StoreValue::Str(s) => (codec::TAG_GIANT_STR, s.as_bytes()),
            StoreValue::Bytes(b) => (codec::TAG_GIANT_BYTES, b),
            _ => return Err(StoreError::Codec("only strings and bytes can be giant")),
        };
        let digest = Sha256::digest(raw);
        let id = match txn.get(self.giant_ids, &digest)? {
            Some(existing) => u64::from_be_bytes(
                existing
                    .try_into()
                    .map_err(|_bad_len| StoreError::Codec("bad giant id"))?,
            ),
            None => {
                let id = self.bump_counter(txn, META_NEXT_GIANT)?;
                let mut stored = vec![giant_tag];
                stored.extend_from_slice(raw);
                txn.put(self.giants, &id.to_be_bytes(), &stored)?;
                txn.put(self.giant_ids, &digest, &id.to_be_bytes())?;
                id
            }
        };
        let mut key = vec![giant_tag];
        key.extend_from_slice(&raw[..codec::GIANT_PREFIX]);
        key.extend_from_slice(&id.to_be_bytes());
        Ok(key)
    }

    fn decode_value(&self, ro: &RoTxn<'_>, bytes: &[u8]) -> Result<(StoreValue, usize)> {
        let (decoded, used) = codec::decode(bytes).map_err(StoreError::Codec)?;
        match decoded {
            codec::Decoded::Value(v) => Ok((v, used)),
            codec::Decoded::Giant { tag, id } => {
                let stored = ro
                    .get(self.giants, &id.to_be_bytes())?
                    .ok_or(StoreError::MissingGiant(id))?;
                let raw = stored
                    .get(1..)
                    .ok_or(StoreError::Codec("empty giant"))?
                    .to_vec();
                let v = if tag == codec::TAG_GIANT_STR {
                    StoreValue::Str(
                        String::from_utf8(raw)
                            .map_err(|_not_utf8| StoreError::Codec("giant not utf8"))?,
                    )
                } else {
                    StoreValue::Bytes(raw)
                };
                Ok((v, used))
            }
        }
    }

    /// Apply `ops` atomically. Cardinality-one adds replace the previous
    /// value; retracts of absent datoms are ignored.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage or codec error; nothing is applied
    /// on failure.
    pub fn transact(&self, ops: &[Op]) -> Result<()> {
        let mut txn = self.env.write_txn()?;
        let mut max_eid = txn
            .get(self.meta, META_MAX_EID)?
            .and_then(|b| b.try_into().ok().map(u64::from_be_bytes))
            .unwrap_or(0);
        for op in ops {
            match op {
                Op::Add { e, a, v } => {
                    let (aid, props) = self.intern_attr(&mut txn, a, None)?;
                    if !props.cardinality_many {
                        // Replace: retract any existing value for (e, a).
                        for existing in self.entity_attr_values(&txn, *e, aid)? {
                            self.delete_datom(&mut txn, *e, aid, &existing)?;
                        }
                    }
                    let venc = self.encode_value_for_key(&mut txn, v)?;
                    self.insert_datom(&mut txn, *e, aid, &venc, v)?;
                    max_eid = max_eid.max(*e);
                }
                Op::Retract { e, a, v } => {
                    let (aid, _props) = self.intern_attr(&mut txn, a, None)?;
                    let venc = self.encode_value_for_key(&mut txn, v)?;
                    self.delete_datom_encoded(&mut txn, *e, aid, &venc, v)?;
                }
            }
        }
        txn.put(self.meta, META_MAX_EID, &max_eid.to_be_bytes())?;
        txn.commit()?;
        Ok(())
    }

    /// All encoded values currently asserted for `(e, aid)`.
    fn entity_attr_values(&self, txn: &RwTxn<'_>, e: u64, aid: u32) -> Result<Vec<Vec<u8>>> {
        let mut prefix = e.to_be_bytes().to_vec();
        prefix.extend_from_slice(&aid.to_be_bytes());
        let mut out = Vec::new();
        for entry in txn.range(self.eav, Some(&prefix), None)? {
            let (k, _) = entry?;
            if !k.starts_with(&prefix) {
                break;
            }
            out.push(k[prefix.len()..].to_vec());
        }
        Ok(out)
    }

    fn insert_datom(
        &self,
        txn: &mut RwTxn<'_>,
        e: u64,
        aid: u32,
        venc: &[u8],
        v: &StoreValue,
    ) -> Result<()> {
        txn.put(self.eav, &eav_key(e, aid, venc), &[])?;
        txn.put(self.ave, &ave_key(aid, venc, e), &[])?;
        if let StoreValue::Ref(target) = v {
            txn.put(self.vae, &vae_key(*target, aid, e), &[])?;
        }
        Ok(())
    }

    fn delete_datom(&self, txn: &mut RwTxn<'_>, e: u64, aid: u32, venc: &[u8]) -> Result<()> {
        txn.del(self.eav, &eav_key(e, aid, venc), None)?;
        txn.del(self.ave, &ave_key(aid, venc, e), None)?;
        // Refs are recognized by their encoding tag.
        if venc.first() == Some(&codec::TAG_REF)
            && let Ok((codec::Decoded::Value(StoreValue::Ref(target)), _)) = codec::decode(venc)
        {
            txn.del(self.vae, &vae_key(target, aid, e), None)?;
        }
        Ok(())
    }

    fn delete_datom_encoded(
        &self,
        txn: &mut RwTxn<'_>,
        e: u64,
        aid: u32,
        venc: &[u8],
        v: &StoreValue,
    ) -> Result<()> {
        txn.del(self.eav, &eav_key(e, aid, venc), None)?;
        txn.del(self.ave, &ave_key(aid, venc, e), None)?;
        if let StoreValue::Ref(target) = v {
            txn.del(self.vae, &vae_key(*target, aid, e), None)?;
        }
        Ok(())
    }

    /// Datoms matching the pattern; `None` components are wildcards.
    /// Index selection mirrors the datalog case tree: bound `e` scans
    /// `eav`, bound `a` scans `ave`, a bound ref value scans `vae`, and
    /// the empty pattern scans everything.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage or codec error.
    pub fn search(
        &self,
        e: Option<u64>,
        a: Option<&str>,
        v: Option<&StoreValue>,
    ) -> Result<Vec<Datom>> {
        let ro = self.env.read_txn()?;
        let aid = match a {
            Some(name) => match self.lookup_aid(name) {
                Some(aid) => Some(aid),
                None => return Ok(Vec::new()),
            },
            None => None,
        };
        let venc = match v {
            Some(value) => Some(self.encode_value_readonly(&ro, value)?),
            None => None,
        };
        let mut out = Vec::new();
        match (e, aid) {
            (Some(e), _) => {
                let mut prefix = e.to_be_bytes().to_vec();
                if let Some(aid) = aid {
                    prefix.extend_from_slice(&aid.to_be_bytes());
                    if let Some(venc) = &venc {
                        prefix.extend_from_slice(venc);
                    }
                }
                self.scan_eav(&ro, &prefix, &mut out)?;
                if aid.is_none()
                    && let Some(want) = v
                {
                    out.retain(|d| &d.v == want);
                }
            }
            (None, Some(aid)) => {
                let mut prefix = aid.to_be_bytes().to_vec();
                if let Some(venc) = &venc {
                    prefix.extend_from_slice(venc);
                }
                self.scan_ave(&ro, &prefix, &mut out)?;
            }
            (None, None) => {
                if let Some(StoreValue::Ref(target)) = v {
                    self.scan_vae(&ro, *target, &mut out)?;
                } else {
                    self.scan_eav(&ro, &[], &mut out)?;
                    if let Some(want) = v {
                        out.retain(|d| &d.v == want);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Encode a value for lookup without allocating a new giant: an
    /// oversized value that was never stored matches nothing.
    fn encode_value_readonly(&self, ro: &RoTxn<'_>, v: &StoreValue) -> Result<Vec<u8>> {
        let mut inline = Vec::new();
        codec::encode_inline(v, &mut inline);
        if inline.len() <= codec::GIANT_THRESHOLD {
            return Ok(inline);
        }
        let (giant_tag, raw): (u8, &[u8]) = match v {
            StoreValue::Str(s) => (codec::TAG_GIANT_STR, s.as_bytes()),
            StoreValue::Bytes(b) => (codec::TAG_GIANT_BYTES, b),
            _ => return Err(StoreError::Codec("only strings and bytes can be giant")),
        };
        let digest = Sha256::digest(raw);
        let id = match ro.get(self.giant_ids, &digest)? {
            Some(existing) => u64::from_be_bytes(
                existing
                    .try_into()
                    .map_err(|_bad_len| StoreError::Codec("bad giant id"))?,
            ),
            // Unknown giant: produce an impossible key (id 0 is never
            // allocated) so exact matches simply miss.
            None => 0,
        };
        let mut key = vec![giant_tag];
        key.extend_from_slice(&raw[..codec::GIANT_PREFIX]);
        key.extend_from_slice(&id.to_be_bytes());
        Ok(key)
    }

    fn lookup_aid(&self, name: &str) -> Option<u32> {
        self.attrs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .map(|(aid, _)| *aid)
    }

    fn attr_name(&self, aid: u32) -> Result<String> {
        self.attr_names
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&aid)
            .cloned()
            .ok_or(StoreError::Codec("unknown aid in index"))
    }

    fn scan_eav(&self, ro: &RoTxn<'_>, prefix: &[u8], out: &mut Vec<Datom>) -> Result<()> {
        let low = if prefix.is_empty() {
            None
        } else {
            Some(prefix)
        };
        for entry in ro.range(self.eav, low, None)? {
            let (k, _) = entry?;
            if !k.starts_with(prefix) {
                break;
            }
            let e = u64::from_be_bytes(
                k.get(..8)
                    .ok_or(StoreError::Codec("short eav key"))?
                    .try_into()
                    .expect("length checked"),
            );
            let aid = u32::from_be_bytes(
                k.get(8..12)
                    .ok_or(StoreError::Codec("short eav key"))?
                    .try_into()
                    .expect("length checked"),
            );
            let (v, _) = self.decode_value(ro, &k[12..])?;
            out.push(Datom {
                e,
                a: self.attr_name(aid)?,
                v,
            });
        }
        Ok(())
    }

    fn scan_ave(&self, ro: &RoTxn<'_>, prefix: &[u8], out: &mut Vec<Datom>) -> Result<()> {
        for entry in ro.range(self.ave, Some(prefix), None)? {
            let (k, _) = entry?;
            if !k.starts_with(prefix) {
                break;
            }
            let aid = u32::from_be_bytes(
                k.get(..4)
                    .ok_or(StoreError::Codec("short ave key"))?
                    .try_into()
                    .expect("length checked"),
            );
            let (v, used) = self.decode_value(ro, &k[4..])?;
            let e = u64::from_be_bytes(
                k.get(4 + used..4 + used + 8)
                    .ok_or(StoreError::Codec("short ave key"))?
                    .try_into()
                    .expect("length checked"),
            );
            out.push(Datom {
                e,
                a: self.attr_name(aid)?,
                v,
            });
        }
        Ok(())
    }

    fn scan_vae(&self, ro: &RoTxn<'_>, target: u64, out: &mut Vec<Datom>) -> Result<()> {
        let prefix = target.to_be_bytes().to_vec();
        for entry in ro.range(self.vae, Some(&prefix), None)? {
            let (k, _) = entry?;
            if !k.starts_with(&prefix) {
                break;
            }
            let aid = u32::from_be_bytes(
                k.get(8..12)
                    .ok_or(StoreError::Codec("short vae key"))?
                    .try_into()
                    .expect("length checked"),
            );
            let e = u64::from_be_bytes(
                k.get(12..20)
                    .ok_or(StoreError::Codec("short vae key"))?
                    .try_into()
                    .expect("length checked"),
            );
            out.push(Datom {
                e,
                a: self.attr_name(aid)?,
                v: StoreValue::Ref(target),
            });
        }
        Ok(())
    }

    /// O(log n) count of datoms matching the pattern, for optimizer
    /// statistics. Patterns that do not map onto one contiguous index
    /// range fall back to `search(...).len()`.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage or codec error.
    pub fn count(&self, e: Option<u64>, a: Option<&str>, v: Option<&StoreValue>) -> Result<u64> {
        let ro = self.env.read_txn()?;
        match (e, a, v) {
            (Some(e), None, None) => self.count_prefix(&ro, self.eav, &e.to_be_bytes()),
            (Some(e), Some(name), value) => {
                let Some(aid) = self.lookup_aid(name) else {
                    return Ok(0);
                };
                let mut prefix = e.to_be_bytes().to_vec();
                prefix.extend_from_slice(&aid.to_be_bytes());
                if let Some(value) = value {
                    prefix.extend_from_slice(&self.encode_value_readonly(&ro, value)?);
                }
                self.count_prefix(&ro, self.eav, &prefix)
            }
            (None, Some(name), value) => {
                let Some(aid) = self.lookup_aid(name) else {
                    return Ok(0);
                };
                let mut prefix = aid.to_be_bytes().to_vec();
                if let Some(value) = value {
                    prefix.extend_from_slice(&self.encode_value_readonly(&ro, value)?);
                }
                self.count_prefix(&ro, self.ave, &prefix)
            }
            (None, None, Some(StoreValue::Ref(target))) => {
                self.count_prefix(&ro, self.vae, &target.to_be_bytes())
            }
            (None, None, None) => Ok(ro.count_all(self.eav)?),
            (e, a, v) => Ok(self.search(e, a, v)?.len() as u64),
        }
    }

    /// Entries whose key starts with `prefix`, as a rank difference:
    /// two O(log n) probes, no scan.
    fn count_prefix(&self, ro: &RoTxn<'_>, dbi: Dbi, prefix: &[u8]) -> Result<u64> {
        let start = ro.count_below(dbi, prefix)?;
        let end = match prefix_successor(prefix) {
            Some(succ) => ro.count_below(dbi, &succ)?,
            None => ro.count_all(dbi)?,
        };
        Ok(end.saturating_sub(start))
    }

    /// Distinct values asserted for an attribute (its ave extent).
    ///
    /// # Errors
    ///
    /// Returns the underlying storage or codec error.
    pub fn cardinality(&self, a: &str) -> Result<u64> {
        self.count(None, Some(a), None)
    }

    /// Up to `n` datoms evenly rank-sampled from an attribute's ave range:
    /// the optimizer's value-distribution probe, O(n log n) total.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage or codec error.
    pub fn sample_ave(&self, a: &str, n: u64) -> Result<Vec<Datom>> {
        let Some(aid) = self.lookup_aid(a) else {
            return Ok(Vec::new());
        };
        let ro = self.env.read_txn()?;
        let prefix = aid.to_be_bytes();
        let start = ro.count_below(self.ave, &prefix)?;
        let total = self.count_prefix(&ro, self.ave, &prefix)?;
        if total == 0 || n == 0 {
            return Ok(Vec::new());
        }
        let take = n.min(total);
        let mut out = Vec::new();
        for i in 0..take {
            let rank = start + (i * total) / take;
            if let Some((k, _)) = ro.get_rank(self.ave, rank)? {
                let key = k.to_vec();
                let (v, used) = self.decode_value(&ro, &key[4..])?;
                let e = u64::from_be_bytes(
                    key.get(4 + used..4 + used + 8)
                        .ok_or(StoreError::Codec("short ave key"))?
                        .try_into()
                        .expect("length checked"),
                );
                out.push(Datom {
                    e,
                    a: a.to_owned(),
                    v,
                });
            }
        }
        Ok(out)
    }

    fn index_dbi(&self, index: Index) -> Dbi {
        match index {
            Index::Eav => self.eav,
            Index::Ave => self.ave,
            Index::Vae => self.vae,
        }
    }

    /// Encode a bound's provided components (contiguous from the index's
    /// most-significant position) into a key prefix. `Ok(None)` means the
    /// bound names an unknown attribute, so the range is empty.
    fn bound_prefix(
        &self,
        ro: &RoTxn<'_>,
        index: Index,
        b: &Bound<'_>,
    ) -> Result<Option<Vec<u8>>> {
        let mut key = Vec::new();
        match index {
            Index::Eav => {
                if let Some(e) = b.e {
                    key.extend_from_slice(&e.to_be_bytes());
                    if let Some(name) = b.a {
                        let Some(aid) = self.lookup_aid(name) else {
                            return Ok(None);
                        };
                        key.extend_from_slice(&aid.to_be_bytes());
                        if let Some(v) = b.v {
                            key.extend_from_slice(&self.encode_value_readonly(ro, v)?);
                        }
                    } else if b.v.is_some() {
                        return Err(StoreError::Codec("eav bound has v without a"));
                    }
                } else if b.a.is_some() || b.v.is_some() {
                    return Err(StoreError::Codec("eav bound has a/v without e"));
                }
            }
            Index::Ave => {
                if let Some(name) = b.a {
                    let Some(aid) = self.lookup_aid(name) else {
                        return Ok(None);
                    };
                    key.extend_from_slice(&aid.to_be_bytes());
                    if let Some(v) = b.v {
                        key.extend_from_slice(&self.encode_value_readonly(ro, v)?);
                        if let Some(e) = b.e {
                            key.extend_from_slice(&e.to_be_bytes());
                        }
                    } else if b.e.is_some() {
                        return Err(StoreError::Codec("ave bound has e without v"));
                    }
                } else if b.v.is_some() || b.e.is_some() {
                    return Err(StoreError::Codec("ave bound has v/e without a"));
                }
            }
            Index::Vae => match b.v {
                Some(StoreValue::Ref(target)) => {
                    key.extend_from_slice(&target.to_be_bytes());
                    if let Some(name) = b.a {
                        let Some(aid) = self.lookup_aid(name) else {
                            return Ok(None);
                        };
                        key.extend_from_slice(&aid.to_be_bytes());
                        if let Some(e) = b.e {
                            key.extend_from_slice(&e.to_be_bytes());
                        }
                    } else if b.e.is_some() {
                        return Err(StoreError::Codec("vae bound has e without a"));
                    }
                }
                Some(_) => return Err(StoreError::Codec("vae bound v must be a ref")),
                None => {
                    if b.a.is_some() || b.e.is_some() {
                        return Err(StoreError::Codec("vae bound has a/e without v"));
                    }
                }
            },
        }
        Ok(Some(key))
    }

    fn decode_index_entry(&self, ro: &RoTxn<'_>, index: Index, k: &[u8]) -> Result<Datom> {
        match index {
            Index::Eav => {
                let e = u64::from_be_bytes(
                    k.get(..8)
                        .ok_or(StoreError::Codec("short eav key"))?
                        .try_into()
                        .expect("length checked"),
                );
                let aid = u32::from_be_bytes(
                    k.get(8..12)
                        .ok_or(StoreError::Codec("short eav key"))?
                        .try_into()
                        .expect("length checked"),
                );
                let (v, _) = self.decode_value(ro, &k[12..])?;
                Ok(Datom {
                    e,
                    a: self.attr_name(aid)?,
                    v,
                })
            }
            Index::Ave => {
                let aid = u32::from_be_bytes(
                    k.get(..4)
                        .ok_or(StoreError::Codec("short ave key"))?
                        .try_into()
                        .expect("length checked"),
                );
                let (v, used) = self.decode_value(ro, &k[4..])?;
                let e = u64::from_be_bytes(
                    k.get(4 + used..4 + used + 8)
                        .ok_or(StoreError::Codec("short ave key"))?
                        .try_into()
                        .expect("length checked"),
                );
                Ok(Datom {
                    e,
                    a: self.attr_name(aid)?,
                    v,
                })
            }
            Index::Vae => {
                let target = u64::from_be_bytes(
                    k.get(..8)
                        .ok_or(StoreError::Codec("short vae key"))?
                        .try_into()
                        .expect("length checked"),
                );
                let aid = u32::from_be_bytes(
                    k.get(8..12)
                        .ok_or(StoreError::Codec("short vae key"))?
                        .try_into()
                        .expect("length checked"),
                );
                let e = u64::from_be_bytes(
                    k.get(12..20)
                        .ok_or(StoreError::Codec("short vae key"))?
                        .try_into()
                        .expect("length checked"),
                );
                Ok(Datom {
                    e,
                    a: self.attr_name(aid)?,
                    v: StoreValue::Ref(target),
                })
            }
        }
    }

    /// Resolve a bound pair into the effective [lo_key, hi_exclusive)
    /// byte range. `Ok(None)` means the range is provably empty.
    #[allow(clippy::type_complexity)]
    fn resolve_range(
        &self,
        ro: &RoTxn<'_>,
        index: Index,
        low: &Bound<'_>,
        high: &Bound<'_>,
    ) -> Result<Option<(Vec<u8>, Option<Vec<u8>>)>> {
        let Some(lo_prefix) = self.bound_prefix(ro, index, low)? else {
            return Ok(None);
        };
        let Some(hi_prefix) = self.bound_prefix(ro, index, high)? else {
            return Ok(None);
        };
        let lo_key = if low.closed || lo_prefix.is_empty() {
            lo_prefix
        } else {
            match prefix_successor(&lo_prefix) {
                Some(s) => s,
                None => return Ok(None),
            }
        };
        let hi_excl = if hi_prefix.is_empty() {
            None
        } else if high.closed {
            prefix_successor(&hi_prefix)
        } else {
            Some(hi_prefix)
        };
        if let Some(hi) = &hi_excl
            && lo_key.as_slice() >= hi.as_slice()
        {
            return Ok(None);
        }
        Ok(Some((lo_key, hi_excl)))
    }

    /// Ordered datoms within an index range given by partial-datom bounds
    /// (inclusive unless a bound is open). Missing bound components mean
    /// +/- infinity. `reverse` walks high-to-low; `limit` caps the result.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage or codec error, or a codec error
    /// for non-contiguous bound components.
    pub fn slice(
        &self,
        index: Index,
        low: &Bound<'_>,
        high: &Bound<'_>,
        limit: Option<usize>,
        reverse: bool,
    ) -> Result<Vec<Datom>> {
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let ro = self.env.read_txn()?;
        let dbi = self.index_dbi(index);
        let Some((lo_key, hi_excl)) = self.resolve_range(&ro, index, low, high)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        if reverse {
            let mut cur = ro.cursor(dbi)?;
            let mut entry = match &hi_excl {
                None => cur.last()?,
                Some(hi) => match cur.set_range(hi)? {
                    Some(_) => cur.prev()?,
                    None => cur.last()?,
                },
            };
            while let Some((k, _)) = entry {
                if k < lo_key.as_slice() {
                    break;
                }
                out.push(self.decode_index_entry(&ro, index, k)?);
                if limit.is_some_and(|n| out.len() >= n) {
                    break;
                }
                entry = cur.prev()?;
            }
        } else {
            let start = if lo_key.is_empty() {
                None
            } else {
                Some(lo_key.as_slice())
            };
            for entry in ro.range(dbi, start, None)? {
                let (k, _) = entry?;
                if let Some(hi) = &hi_excl
                    && k >= hi.as_slice()
                {
                    break;
                }
                out.push(self.decode_index_entry(&ro, index, k)?);
                if limit.is_some_and(|n| out.len() >= n) {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// O(log n) count of datoms within an index range given by
    /// partial-datom bounds, via rank differences.
    ///
    /// # Errors
    ///
    /// Returns the underlying storage or codec error, or a codec error
    /// for non-contiguous bound components.
    pub fn count_range(&self, index: Index, low: &Bound<'_>, high: &Bound<'_>) -> Result<u64> {
        let ro = self.env.read_txn()?;
        let dbi = self.index_dbi(index);
        let Some((lo_key, hi_excl)) = self.resolve_range(&ro, index, low, high)? else {
            return Ok(0);
        };
        let start = if lo_key.is_empty() {
            0
        } else {
            ro.count_below(dbi, &lo_key)?
        };
        let end = match &hi_excl {
            Some(hi) => ro.count_below(dbi, hi)?,
            None => ro.count_all(dbi)?,
        };
        Ok(end.saturating_sub(start))
    }
}

/// Index selector for range scans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Index {
    Eav,
    Ave,
    Vae,
}

/// A partial-datom range bound. Components must be contiguous from the
/// index's most-significant position; a missing tail means +/- infinity.
/// `closed` marks the bound inclusive (the default shape).
pub struct Bound<'a> {
    pub e: Option<u64>,
    pub a: Option<&'a str>,
    pub v: Option<&'a StoreValue>,
    pub closed: bool,
}

impl Default for Bound<'_> {
    fn default() -> Self {
        Bound {
            e: None,
            a: None,
            v: None,
            closed: true,
        }
    }
}

fn eav_key(e: u64, aid: u32, venc: &[u8]) -> Vec<u8> {
    let mut k = e.to_be_bytes().to_vec();
    k.extend_from_slice(&aid.to_be_bytes());
    k.extend_from_slice(venc);
    k
}

fn ave_key(aid: u32, venc: &[u8], e: u64) -> Vec<u8> {
    let mut k = aid.to_be_bytes().to_vec();
    k.extend_from_slice(venc);
    k.extend_from_slice(&e.to_be_bytes());
    k
}

fn vae_key(target: u64, aid: u32, e: u64) -> Vec<u8> {
    let mut k = target.to_be_bytes().to_vec();
    k.extend_from_slice(&aid.to_be_bytes());
    k.extend_from_slice(&e.to_be_bytes());
    k
}

/// The smallest byte string greater than every string with this prefix,
/// or `None` when the prefix is all 0xFF.
fn prefix_successor(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut succ = prefix.to_vec();
    while let Some(last) = succ.last_mut() {
        if *last == 0xFF {
            succ.pop();
        } else {
            *last += 1;
            return Some(succ);
        }
    }
    None
}
