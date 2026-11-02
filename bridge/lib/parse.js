'use strict';
/**
 * 把页面旁路抓到的一帧翻成结构化事件。
 *
 * 两种帧形态（见 docs/research-notes.md）：
 *   1. 注入脚本已经解好的：{ e: 'text-delta'|'reasoning-delta'|'tool-input-available'|'finish'|'error', d, n, i }
 *   2. 原始 SSE 行：{ l: 'a0:"你好"' } / { l: 'data: {"records":[...]}' }
 *
 * 输入种类是有限且明确的，所以这里做成纯函数，node:test 直接覆盖，
 * 以后 Arena 改帧格式只需要改这个文件 + 跑 npm test，不用再连真机猜。
 */

const EMPTY = () => ({
  out: false,
  events: [],
  done: false,
  routed: null,
  error: null,
});

function textEvent(text) {
  return { type: 'text', text };
}
function reasoningEvent(text) {
  return { type: 'reasoning', text };
}

// 解析 "data: {records:[{body}]}" 里的一条 record，返回事件或 null
function parseRecord(rec) {
  let b;
  try {
    b = JSON.parse(rec && rec.body);
  } catch (e) {
    return null;
  }
  const d = (b && b.data) || {};
  switch (d.type) {
    case 'text-delta':
      return { type: 'text', text: d.delta || '' };
    case 'reasoning-delta':
      return { type: 'reasoning', text: d.delta || '' };
    case 'tool-input-available':
      return { type: 'tool', name: d.toolName, input: d.input };
    case 'finish':
      return { type: 'finish' };
    case 'error':
      return { type: 'error', message: JSON.stringify(d).slice(0, 200) };
    default:
      return null;
  }
}

/** 解析一帧，永不抛异常（错误也以 error 字段返回，由调用方决定怎么处理）。 */
function parseFrame(f) {
  const r = EMPTY();
  if (!f) return r;

  // 形态 1：注入脚本解好的结构化帧
  if (f.e) {
    const t = f.d || '';
    switch (f.e) {
      case 'text-delta':
        r.out = true;
        if (t) r.events.push(textEvent(t));
        return r;
      case 'reasoning-delta':
        r.out = true;
        if (t) r.events.push(reasoningEvent(t));
        return r;
      case 'tool-input-available':
        r.out = true;
        r.events.push({ type: 'tool', name: f.n, input: f.i });
        return r;
      case 'finish':
        r.done = true;
        return r;
      case 'error':
        r.error = 'agent: ' + JSON.stringify(f).slice(0, 300);
        return r;
      default:
        return r;
    }
  }

  // 形态 2：原始 SSE 行
  const line = f.l || '';
  if (!line) return r;

  if (line.startsWith('a0:')) {
    let d;
    try {
      d = JSON.parse(line.slice(3));
    } catch (e) {
      d = line.slice(3).replace(/^"|"$/g, '');
    }
    r.out = true;
    if (d) r.events.push(textEvent(typeof d === 'string' ? d : String(d)));
    return r;
  }
  if (line.startsWith('ag:')) {
    let d;
    try {
      d = JSON.parse(line.slice(3));
    } catch (e) {
      d = line.slice(3);
    }
    r.out = true;
    if (d) r.events.push(reasoningEvent(typeof d === 'string' ? d : String(d)));
    return r;
  }
  if (line.startsWith('a2:')) {
    try {
      const a = JSON.parse(line.slice(3));
      if (Array.isArray(a) && a[0] && a[0].type === 'routed_model')
        r.routed = a[0].organization || '';
    } catch (e) {
      /* 心跳或格式变了，忽略 */
    }
    return r;
  }
  if (line.startsWith('a3:')) {
    let m = line.slice(3);
    try {
      m = JSON.parse(m);
    } catch (e) {
      /* 保留原文 */
    }
    r.error = 'upstream: ' + String(m).slice(0, 300);
    return r;
  }
  if (line.startsWith('ad:')) {
    r.done = true;
    return r;
  }
  if (line.startsWith('data: ')) {
    let o;
    try {
      o = JSON.parse(line.slice(6));
    } catch (e) {
      return r;
    }
    if (!o || !Array.isArray(o.records)) return r;
    for (const rec of o.records) {
      const ev = parseRecord(rec);
      if (!ev) continue;
      if (ev.type === 'finish') {
        r.done = true;
        continue;
      }
      if (ev.type === 'error') {
        r.error = 'Arena rejected the turn, please resend: ' + ev.message;
        continue;
      }
      r.out = true;
      r.events.push(ev);
    }
    return r;
  }
  return r;
}

module.exports = { parseFrame, parseRecord };
