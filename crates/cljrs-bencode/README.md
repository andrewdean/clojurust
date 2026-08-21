# cljrs-bencode

**Purpose:** Minimal incremental bencode codec, extracted from `cljrs-nrepl` so the nREPL server and the cljrsh babashka-pod protocol client share one implementation.

**Status:** Implemented (moved verbatim from `cljrs-nrepl/src/bencode.rs`; `cljrs-nrepl` re-exports it as `cljrs_nrepl::bencode` for compatibility). Part of the cljrsh scripting-binary work.

## File layout

- `src/lib.rs` — the whole codec: `Bencode` value type, streaming-friendly decoder, canonical encoder, unit tests.

## Public API

- `enum Bencode { Int(i64), Bytes(Vec<u8>), List(Vec<Bencode>), Dict(BTreeMap<Vec<u8>, Bencode>) }` — dictionary keys are byte strings; `BTreeMap` yields canonical (sorted) encoding order.
  - `Bencode::str(impl AsRef<str>) -> Bencode` — byte string from UTF-8 text.
  - `Bencode::as_str(&self) -> Option<&str>`, `Bencode::as_dict(&self) -> Option<&BTreeMap<Vec<u8>, Bencode>>` — accessors.
- `enum BencodeError { Invalid(&'static str) }` — decode error for input that cannot be valid bencode.
- `fn encode(v: &Bencode, out: &mut Vec<u8>)` / `fn encode_to_vec(v: &Bencode) -> Vec<u8>` — canonical encoding.
- `fn decode(buf: &[u8]) -> Result<Option<(Bencode, usize)>, BencodeError>` — decode one value from the front of `buf`. `Ok(Some((value, consumed)))` on success, `Ok(None)` when the buffer holds only a prefix of a value (read more bytes and retry — this is the framing primitive for TCP/stdio read loops), `Err` for malformed input.

Only the four core bencode types are supported, which is all the nREPL and babashka-pod wire protocols use.
