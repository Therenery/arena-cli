#!/usr/bin/env node
/* 测试用的假桥：只实现 arena-lite 用到的几个端点，用来做端到端验证。 */
const http = require('http');

const PORT = Number(process.env.PORT || 17899);
let history = [];
let verifyLeft = Number(process.env.VERIFY_TIMES || 0);

const sse = (res) => {
  res.writeHead(200, {
    'Content-Type': 'text/event-stream; charset=utf-8',
    'Cache-Control': 'no-cache',
    Connection: 'keep-alive',
  });
};

http
  .createServer((req, res) => {
    const u = new URL(req.url, 'http://x');
    let body = '';
    req.on('data', (d) => (body += d));
    req.on('end', () => {
      if (u.pathname === '/health') {
        const s = JSON.stringify({
          ok: true,
          chrome: true,
          version: '0.4.0-mock',
          uptime: 12,
          sessions: ['main', '设计'],
          queue: 0,
          working: false,
          limits: { maxQueue: 4, maxSessions: 6, minGapMs: 3000, maxPromptChars: 6000 },
          auth: false,
        });
        res.writeHead(200, { 'Content-Type': 'application/json' });
        return res.end(s);
      }
      if (u.pathname === '/models') {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        return res.end(JSON.stringify({ models: ['auto', 'max'] }));
      }
      if (u.pathname === '/history') {
        const name = u.searchParams.get('session') || 'main';
        if (name !== 'main' && name !== '设计') {
          res.writeHead(404, { 'Content-Type': 'application/json' });
          return res.end(JSON.stringify({ ok: false, error: 'no session' }));
        }
        res.writeHead(200, { 'Content-Type': 'application/json' });
        return res.end(
          JSON.stringify({
            ok: true,
            session: name,
            at: Date.now() - 60000,
            turns: 2,
            messages: [
              { role: 'user', text: '这是网页端的第一句' },
              { role: 'assistant', text: '这是网页端的第一句回答' },
            ],
          }),
        );
      }
      if (u.pathname === '/v1/chat/completions') {
        const parsed = JSON.parse(body || '{}');
        console.log('v1 msgs=' + (parsed.messages || []).length + ' mode=' +
          (parsed.metadata && parsed.metadata.mode) + ' user=' + parsed.user);
        const last = (parsed.messages || []).slice(-1)[0] || {};
        const text = String(last.content || '');
        if (text.indexOf('QUEUE') >= 0) {
          res.writeHead(429, { 'Content-Type': 'application/json' });
          return res.end(JSON.stringify({ error: { message: 'busy: queue full' } }));
        }
        if (text.indexOf('VERIFY') >= 0 && verifyLeft > 0) {
          verifyLeft -= 1;
          res.writeHead(409, { 'Content-Type': 'application/json' });
          return res.end(JSON.stringify({ error: { message: 'NEED_VERIFY' } }));
        }
        sse(res);
        const chunk = (delta) =>
          res.write(
            'data: ' +
              JSON.stringify({
                id: 'chatcmpl-x',
                object: 'chat.completion.chunk',
                created: 0,
                model: parsed.model || 'arena-agent',
                choices: [{ index: 0, delta, finish_reason: null }],
              }) +
              '\n\n',
          );
        chunk({ reasoning_content: '\n· 先看看目录结构\n' });
        chunk({ reasoning_content: '· read_file {"path":"src/main.rs"}\n' });
        chunk({ reasoning_content: '再决定读哪个文件。\n' });
        const parts = ['你好，', '我是 arena，', '这是流式输出。'];
        if (text.indexOf('SLOW') >= 0) {
          const chunk2 = (delta) => res.write('data: ' + JSON.stringify({ choices: [{ index: 0, delta }] }) + '\n\n');
          let k = 0;
          const t2 = setInterval(() => {
            if (k++ < 60) chunk2({ content: '第' + k + '段 ' });
            else { clearInterval(t2); res.write('data: [DONE]\n\n'); res.end(); }
          }, 100);
          return;
        }
        if (parsed.metadata && parsed.metadata.mode === 'battle') {
          parts.push('\n\n**A · gpt**\n\n答案甲\n\n---\n\n**B · claude**\n\n答案乙');
        }
        let i = 0;
        const timer = setInterval(() => {
          if (i < parts.length) {
            chunk({ content: parts[i++] });
          } else {
            clearInterval(timer);
            res.write('data: [DONE]\n\n');
            res.end();
            history.push({ role: 'user', text });
            history.push({ role: 'assistant', text: parts.join('') });
          }
        }, 30);
        return;
      }
      res.writeHead(404, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'not found' }));
    });
  })
  .listen(PORT, '127.0.0.1', () => console.log('mock bridge on ' + PORT));
