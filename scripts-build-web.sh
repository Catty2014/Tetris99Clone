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
mkdir -p crates/t99-client/web/assets/fonts
cp crates/t99-client/assets/fonts/CJK.otf crates/t99-client/web/assets/fonts/CJK.otf
cat > crates/t99-client/web/index.html <<'HTML'
<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>t99 · Arena Tetris</title>
<style>
  html, body { margin: 0; height: 100%; background: #0b0d12; overflow: hidden; }
  canvas { display: block; width: 100%; height: 100%; }
</style>
</head>
<body>
<script>
(function () {
  // 后备缓冲上限：wgpu 配置 surface 时若 width/height 超过 GL 的 MAX_TEXTURE_SIZE 会 panic，
  // 画面全黑。Firefox 开 privacy.resistFingerprinting 后会改写 devicePixelRatio 与视口尺寸，
  // 高 dpr + 小显存/软件渲染（上限 2048）就会命中。这里按下限压低 dpr，保证物理像素不越界。
  var cap = 4096;
  try {
    var probe = document.createElement('canvas');
    var gl = probe.getContext('webgl2') || probe.getContext('webgl');
    if (gl) cap = gl.getParameter(gl.MAX_TEXTURE_SIZE) || cap;
  } catch (e) {}
  var realDpr = window.devicePixelRatio || 1;
  function cappedDpr() {
    var w = window.innerWidth || 1;
    var h = window.innerHeight || 1;
    var lim = Math.min(cap / w, cap / h);
    return Math.max(0.1, Math.min(realDpr, Math.floor(lim * 100) / 100));
  }
  try {
    Object.defineProperty(window, 'devicePixelRatio', {
      configurable: true,
      get: cappedDpr,
    });
  } catch (e) {
    try { window.devicePixelRatio = cappedDpr(); } catch (e2) {}
  }
  // wasm 里的 panic 走的是回调，init().catch 抓不到；这里兜一层可见的错误提示。
  function showError(err) {
    if (document.getElementById('t99-err')) return;
    var pre = document.createElement('pre');
    pre.id = 't99-err';
    pre.style.cssText =
      'color:#f66;padding:16px;white-space:pre-wrap;font:12px/1.4 monospace;max-height:100%;overflow:auto';
    pre.textContent = String(err);
    document.body.appendChild(pre);
  }
  window.addEventListener('error', function (e) {
    showError(e.message || e.error || 'unknown error');
  });
  window.addEventListener('unhandledrejection', function (e) {
    showError(e.reason || 'unhandled rejection');
  });
})();
</script>
<script type="module">
  import init from './t99-client.js';
  // ?config=<url> / ?ruleset=<url>：在启动前把配置取回并交给 Rust（避免在 wasm 里做异步 IO）
  const q = new URLSearchParams(location.search);
  try {
    if (q.get('config')) window.__t99_config = await (await fetch(q.get('config'))).text();
    if (q.get('ruleset')) window.__t99_ruleset = await (await fetch(q.get('ruleset'))).text();
  } catch (e) {
    console.warn('[t99] 配置预取失败，使用内嵌配置:', e);
  }
  init().catch((e) => {
    document.body.innerHTML = '<pre style="color:#f66;padding:16px">' + e + '</pre>';
    throw e;
  });
</script>
</body>
</html>
HTML
echo "完成: cd crates/t99-client/web && python3 -m http.server 8137"
