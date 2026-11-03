'use strict';
const test = require('node:test');
const assert = require('node:assert');
const { HISTORY_JS } = require('../lib/history');

/* 一个够用的假 DOM：只要 HISTORY_JS 用到的那几个方法。 */

function el(opts) {
  const node = {
    innerText: opts.text || '',
    textContent: opts.text || '',
    children: opts.children || [],
    attrs: opts.attrs || {},
    rect: opts.rect || { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 },
    getAttribute(name) {
      return Object.prototype.hasOwnProperty.call(this.attrs, name)
        ? this.attrs[name]
        : null;
    },
    getBoundingClientRect() {
      return this.rect;
    },
    contains(other) {
      if (other === this) return true;
      return this.children.some(
        (c) => c === other || (c.contains && c.contains(other)),
      );
    },
  };
  return node;
}

function run(doc) {
  // HISTORY_JS 是一段自执行表达式，直接求值即可
  const fn = new Function('document', 'return ' + HISTORY_JS);
  const raw = fn(doc);
  return JSON.parse(raw);
}

test('带角色属性时按属性取消息', () => {
  const doc = {
    querySelectorAll: () => [
      el({ text: '你好', attrs: { 'data-message-author-role': 'user' } }),
      el({ text: '你好，有什么可以帮你', attrs: { 'data-message-author-role': 'assistant' } }),
    ],
    querySelector: () => null,
  };
  const r = run(doc);
  assert.equal(r.ok, true);
  assert.equal(r.via, 'role-attrs');
  assert.deepEqual(r.messages, [
    { role: 'user', text: '你好' },
    { role: 'assistant', text: '你好，有什么可以帮你' },
  ]);
});

test('data-testid 也能认出角色，嵌套节点只留最外层', () => {
  const inner = el({ text: '内层重复', attrs: { 'data-testid': 'user-message-body' } });
  const outer = el({
    text: '外层整条',
    attrs: { 'data-testid': 'user-message' },
    children: [inner],
  });
  const doc = {
    querySelectorAll: () => [outer, inner],
    querySelector: () => null,
  };
  const r = run(doc);
  assert.equal(r.ok, true);
  assert.deepEqual(r.messages, [{ role: 'user', text: '外层整条' }]);
});

test('没有角色属性时按气泡左右对齐判断', () => {
  const box = { left: 0, top: 0, right: 800, bottom: 600, width: 800, height: 600 };
  const log = el({
    text: '',
    rect: box,
    children: [
      el({ text: '用户问的问题', rect: { left: 500, top: 0, right: 790, bottom: 40, width: 290, height: 40 } }),
      el({ text: '助手的长回答', rect: { left: 10, top: 40, right: 790, bottom: 200, width: 780, height: 160 } }),
    ],
  });
  const doc = {
    querySelectorAll: () => [],
    querySelector: (sel) => (sel.indexOf('log') >= 0 ? log : null),
  };
  const r = run(doc);
  assert.equal(r.ok, true);
  assert.equal(r.via, 'bubble-align');
  assert.deepEqual(r.messages, [
    { role: 'user', text: '用户问的问题' },
    { role: 'assistant', text: '助手的长回答' },
  ]);
});

test('全都认不出时退化成交替，并标明 via', () => {
  const box = { left: 0, top: 0, right: 800, bottom: 600, width: 800, height: 600 };
  const children = [
    el({ text: '一', rect: { ...box } }),
    el({ text: '二', rect: { ...box } }),
    el({ text: '三', rect: { ...box } }),
  ];
  const log = el({ text: '', rect: box, children });
  const doc = {
    querySelectorAll: () => [],
    querySelector: () => log,
  };
  const r = run(doc);
  assert.equal(r.ok, true);
  assert.equal(r.via, 'alternate');
  assert.deepEqual(
    r.messages.map((m) => m.role),
    ['user', 'assistant', 'user'],
  );
});

test('页面里什么都没有时明确返回 ok:false，不编内容', () => {
  const doc = {
    querySelectorAll: () => [],
    querySelector: () => null,
  };
  const r = run(doc);
  assert.equal(r.ok, false);
  assert.equal(r.messages, undefined);
  assert.match(r.error, /no message nodes/);
});

test('连续重复的同角色文本会被合并', () => {
  const doc = {
    querySelectorAll: () => [
      el({ text: '同一句', attrs: { 'data-role': 'assistant' } }),
      el({ text: '同一句', attrs: { 'data-role': 'assistant' } }),
    ],
    querySelector: () => null,
  };
  const r = run(doc);
  assert.equal(r.messages.length, 1);
});

test('DOM 抛异常时也返回 JSON，而不是炸掉调用方', () => {
  const doc = {
    querySelectorAll: () => {
      throw new Error('boom');
    },
    querySelector: () => null,
  };
  const r = run(doc);
  assert.equal(r.ok, false);
  assert.match(r.error, /boom/);
});
