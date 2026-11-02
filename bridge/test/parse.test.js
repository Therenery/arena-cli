'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { parseFrame } = require('../lib/parse');

/* ---------- direct 模式（create-evaluation / create-chat） ---------- */

test('a0 帧是正文', () => {
  const r = parseFrame({ l: 'a0:"你好"' });
  assert.equal(r.error, null);
  assert.equal(r.out, true);
  assert.deepEqual(r.events, [{ type: 'text', text: '你好' }]);
});

test('a0 帧 JSON 坏掉时退化成去引号的原文', () => {
  const r = parseFrame({ l: 'a0:半截内容' });
  assert.deepEqual(r.events, [{ type: 'text', text: '半截内容' }]);
});

test('a0 帧是空字符串时不产出事件（但算有输出）', () => {
  const r = parseFrame({ l: 'a0:""' });
  assert.deepEqual(r.events, []);
  assert.equal(r.out, true);
});

test('ag 帧是思考内容', () => {
  const r = parseFrame({ l: 'ag:"先看一下仓库结构"' });
  assert.deepEqual(r.events, [{ type: 'reasoning', text: '先看一下仓库结构' }]);
});

test('a2 帧是路由信息，不是正文', () => {
  const r = parseFrame({
    l: 'a2:[{"type":"routed_model","organization":"anthropic"}]',
  });
  assert.equal(r.routed, 'anthropic');
  assert.deepEqual(r.events, []);
  assert.equal(r.out, false);
});

test('a2 帧格式变了也不炸', () => {
  const r = parseFrame({ l: 'a2:not-json' });
  assert.equal(r.error, null);
  assert.equal(r.routed, null);
});

test('a3 帧是上游错误', () => {
  const r = parseFrame({ l: 'a3:"boom"' });
  assert.equal(r.error, 'upstream: boom');
  assert.deepEqual(r.events, []);
});

test('a3 帧不是 JSON 时保留原文', () => {
  const r = parseFrame({ l: 'a3:rate limited' });
  assert.equal(r.error, 'upstream: rate limited');
});

test('ad 帧是结束', () => {
  const r = parseFrame({ l: 'ad:' });
  assert.equal(r.done, true);
});

/* ---------- agent 模式（ai-proxy/realtime，SSE batch） ---------- */

test('注入脚本解好的 text-delta', () => {
  const r = parseFrame({ e: 'text-delta', d: 'ab' });
  assert.deepEqual(r.events, [{ type: 'text', text: 'ab' }]);
  assert.equal(r.out, true);
});

test('注入脚本解好的 reasoning-delta', () => {
  const r = parseFrame({ e: 'reasoning-delta', d: '嗯' });
  assert.deepEqual(r.events, [{ type: 'reasoning', text: '嗯' }]);
});

test('tool-input-available 带出工具名和入参', () => {
  const r = parseFrame({
    e: 'tool-input-available',
    n: 'bash',
    i: { command: 'ls' },
  });
  assert.deepEqual(r.events, [
    { type: 'tool', name: 'bash', input: { command: 'ls' } },
  ]);
});

test('finish 帧是结束', () => {
  assert.equal(parseFrame({ e: 'finish' }).done, true);
});

test('error 帧带上 agent 前缀', () => {
  const r = parseFrame({ e: 'error', d: 'quota' });
  assert.match(r.error, /^agent: /);
  assert.match(r.error, /quota/);
});

test('data: records 里的 text-delta 累积成事件', () => {
  const line =
    'data: ' +
    JSON.stringify({
      records: [
        { body: JSON.stringify({ data: { type: 'text-delta', delta: '你' } }) },
        { body: JSON.stringify({ data: { type: 'text-delta', delta: '好' } }) },
        { body: JSON.stringify({ data: { type: 'finish' } }) },
      ],
    });
  const r = parseFrame({ l: line });
  assert.deepEqual(r.events, [
    { type: 'text', text: '你' },
    { type: 'text', text: '好' },
  ]);
  assert.equal(r.done, true);
  assert.equal(r.out, true);
});

test('records 里的 tool-input-available 映射成 tool 事件', () => {
  const line =
    'data: ' +
    JSON.stringify({
      records: [
        {
          body: JSON.stringify({
            data: {
              type: 'tool-input-available',
              toolName: 'read_file',
              input: { path: 'a.js' },
            },
          }),
        },
      ],
    });
  const r = parseFrame({ l: line });
  assert.deepEqual(r.events, [
    { type: 'tool', name: 'read_file', input: { path: 'a.js' } },
  ]);
});

test('records 里坏掉的 body 跳过，不影响后面的帧', () => {
  const line =
    'data: ' +
    JSON.stringify({
      records: [
        { body: '{not json' },
        { body: JSON.stringify({ data: { type: 'text-delta', delta: 'ok' } }) },
      ],
    });
  const r = parseFrame({ l: line });
  assert.deepEqual(r.events, [{ type: 'text', text: 'ok' }]);
});

test('records 里的 error 变成可读错误', () => {
  const line =
    'data: ' +
    JSON.stringify({
      records: [
        { body: JSON.stringify({ data: { type: 'error', message: 'nope' } }) },
      ],
    });
  const r = parseFrame({ l: line });
  assert.match(r.error, /Arena rejected the turn, please resend/);
});

/* ---------- 其它 ---------- */

test('空帧、心跳、未知行都不产出任何东西也不抛', () => {
  for (const f of [
    null,
    undefined,
    {},
    { l: '' },
    { l: ': ping' },
    { l: 'data: {"nope":1}' },
    { l: 'z9:x' },
  ]) {
    const r = parseFrame(f);
    assert.equal(r.error, null);
    assert.equal(r.done, false);
    assert.deepEqual(r.events, []);
  }
});
