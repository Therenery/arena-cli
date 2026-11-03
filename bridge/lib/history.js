'use strict';
/**
 * /history 用的页面 DOM 读取脚本。
 *
 * 单独放在 lib/ 里是为了能用 node:test 拿假 DOM 覆盖：
 * 页面结构一变，先跑 npm test 就能看出来，不用连真机猜。
 *
 * 三层启发式，依次退化：
 *   1. role-attrs   —— 节点带 data-role / data-message-author-role
 *   2. bubble-align —— 按「气泡靠右的是用户」判断
 *   3. alternate    —— 兜底的 user/assistant 交替（客户端会提示「顺序是猜的」）
 * 一层都匹配不上就返回 ok:false，绝不编造内容。
 */
const HISTORY_JS = `(() => {
  try {
    const out = [];
    const MAX = 200;
    const txt = (el) => String((el && (el.innerText || el.textContent)) || '').trim();
    const push = (role, text) => {
      text = String(text || '').trim();
      if (!text || out.length >= MAX) return;
      const last = out[out.length - 1];
      if (last && last.role === role && (last.text === text || last.text.indexOf(text) === 0)) {
        if (text.length >= last.text.length) last.text = text;
        return;
      }
      out.push({ role: role, text: text });
    };

    // 1) 角色属性最可靠
    const sel = '[data-message-author-role],[data-role="user"],[data-role="assistant"],' +
      '[data-testid*="user-message"],[data-testid*="assistant-message"],[data-testid*="chat-message"]';
    let nodes = [].slice.call(document.querySelectorAll(sel));
    nodes = nodes.filter((n) => !nodes.some((m) => m !== n && m.contains(n)));
    for (const n of nodes) {
      let role = n.getAttribute('data-message-author-role') || n.getAttribute('data-role') || '';
      if (!role) {
        const t = String(n.getAttribute('data-testid') || '').toLowerCase();
        role = t.indexOf('user') >= 0 ? 'user'
          : (t.indexOf('assistant') >= 0 || t.indexOf('bot') >= 0 || t.indexOf('model') >= 0) ? 'assistant'
          : '';
      }
      if (/assistant|model|bot|arena/i.test(role)) role = 'assistant';
      else if (/user|human|you/i.test(role)) role = 'user';
      else continue;
      push(role, txt(n));
    }
    if (out.length) return JSON.stringify({ ok: true, via: 'role-attrs', messages: out });

    const log = document.querySelector('[role="log"]') || document.querySelector('main');
    if (log) {
      // 2) 按「谁靠右谁是用户」判气泡
      const box = log.getBoundingClientRect();
      let kids = [].slice.call(log.children).filter((e) => txt(e).length > 0);
      if (box && box.width > 0 && kids.length) {
        for (const k of kids) {
          const r = k.getBoundingClientRect();
          const rightish =
            r.width > 0 && r.left > box.left + box.width * 0.25 && r.right > box.left + box.width * 0.7;
          push(rightish ? 'user' : 'assistant', txt(k));
        }
        if (out.length && out.some((m) => m.role === 'user')) {
          return JSON.stringify({ ok: true, via: 'bubble-align', messages: out });
        }
        out.length = 0;
      }
      // 3) 最后手段：交替。客户端会提示「顺序是猜的」
      kids = [].slice.call(log.children).filter((e) => txt(e).length > 0);
      if (kids.length >= 2) {
        kids.forEach((k, i) => push(i % 2 === 0 ? 'user' : 'assistant', txt(k)));
        if (out.length) return JSON.stringify({ ok: true, via: 'alternate', messages: out });
      }
    }
    return JSON.stringify({ ok: false, error: 'no message nodes matched (页面结构可能变了)' });
  } catch (e) {
    return JSON.stringify({ ok: false, error: String((e && e.message) || e) });
  }
})()`;

module.exports = { HISTORY_JS };
