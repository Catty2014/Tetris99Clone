#!/usr/bin/env bash
# 构建浏览器版：wasm 编译 + wasm-bindgen 胶水 + 剥离 name 段
set -e
cd "$(dirname "$0")"
cargo build -p t99-client --release --target wasm32-unknown-unknown
wasm-bindgen --out-dir crates/t99-client/web --target web target/wasm32-unknown-unknown/release/t99-client.wasm
python3 - <<'PY'
def read_uleb(d, p):
    r = 0; s = 0
    while True:
        b = d[p]; p += 1
        r |= (b & 0x7f) << s
        if not (b & 0x80): break
        s += 7
    return r, p
data = open('crates/t99-client/web/t99-client_bg.wasm','rb').read()
out = bytearray(data[:8]); pos = 8
while pos < len(data):
    sec_id = data[pos]; pos += 1
    size, p2 = read_uleb(data, pos)
    body = data[p2:p2+size]
    keep = True
    if sec_id == 0:
        name_len, q = read_uleb(body, 0)
        keep = body[q:q+name_len] != b'name'
    if keep:
        out.append(sec_id); out += data[pos:p2]; out += body
    pos = p2 + size
open('crates/t99-client/web/t99-client_bg.wasm','wb').write(bytes(out))
print(f"strip -> {len(out)/1e6:.1f}MB")
PY
cp crates/t99-client/assets/fonts/CJK.otf crates/t99-client/web/assets/fonts/CJK.otf
echo "完成: cd crates/t99-client/web && python3 -m http.server 8137"
