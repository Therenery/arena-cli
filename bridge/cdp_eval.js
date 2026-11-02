const http = require('http');
const b64 = process.argv[2] || '';
const expr = b64
  ? Buffer.from(b64, 'base64').toString('utf8')
  : 'document.title';
function get(p) {
  return new Promise((res, rej) => {
    http
      .get({ host: '127.0.0.1', port: 9222, path: p }, (r) => {
        let b = '';
        r.on('data', (d) => (b += d));
        r.on('end', () => {
          try {
            res(JSON.parse(b));
          } catch (e) {
            rej(e);
          }
        });
      })
      .on('error', rej);
  });
}
(async () => {
  const list = await get('/json/list');
  const t =
    list.filter((x) => x.type === 'page' && /arena\.ai/.test(x.url))[0] ||
    list.filter((x) => x.type === 'page')[0];
  if (!t) {
    console.log('NO_TAB');
    return;
  }
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  let id = 0;
  const pend = new Map();
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.id && pend.has(m.id)) {
      const p = pend.get(m.id);
      pend.delete(m.id);
      p(m);
    }
  };
  const send = (method, params) =>
    new Promise((r) => {
      const i = ++id;
      pend.set(i, r);
      ws.send(JSON.stringify({ id: i, method, params }));
    });
  await new Promise((r) => (ws.onopen = r));
  const r = await send('Runtime.evaluate', {
    expression: expr,
    returnByValue: true,
    awaitPromise: true,
  });
  const v = r.result && r.result.result ? r.result.result.value : r;
  console.log(typeof v === 'string' ? v : JSON.stringify(v));
  ws.close();
})();
