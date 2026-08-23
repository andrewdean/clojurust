#!/usr/bin/env python3
"""Minimal nREPL client: eval one expression against a port, print the value.
Stands in for CIDER in the shared-environment proof."""
import socket, sys

def bencode(v):
    if isinstance(v, dict):
        return b"d" + b"".join(bencode(k) + bencode(x) for k, x in sorted(v.items())) + b"e"
    if isinstance(v, str):
        b = v.encode()
        return str(len(b)).encode() + b":" + b
    raise TypeError(v)

def bdecode(buf, i=0):
    c = buf[i:i+1]
    if c == b"d":
        i += 1; d = {}
        while buf[i:i+1] != b"e":
            k, i = bdecode(buf, i); v, i = bdecode(buf, i); d[k] = v
        return d, i + 1
    if c == b"l":
        i += 1; l = []
        while buf[i:i+1] != b"e":
            v, i = bdecode(buf, i); l.append(v)
        return l, i + 1
    if c == b"i":
        j = buf.index(b"e", i)
        return int(buf[i+1:j]), j + 1
    j = buf.index(b":", i)
    n = int(buf[i:j])
    return buf[j+1:j+1+n].decode(), j + 1 + n

port, code = int(sys.argv[1]), sys.argv[2]
s = socket.create_connection(("127.0.0.1", port), timeout=30)
s.sendall(bencode({"op": "eval", "code": code, "id": "py1"}))
buf = b""
while True:
    buf += s.recv(65536)
    try:
        while buf:
            msg, used = bdecode(buf)
            buf = buf[used:]
            if "value" in msg:
                print(msg["value"])
            if "done" in msg.get("status", []):
                sys.exit(0)
    except (ValueError, IndexError):
        continue  # incomplete frame; read more
