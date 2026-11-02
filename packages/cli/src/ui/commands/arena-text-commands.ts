/**
 * @license
 * Copyright 2026 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * Arena 文本模式：Battle / Side by Side / Direct。
 *
 * 页面由 arena-bridge 驱动，CLI 只负责发问、展示两边答案、收集投票。
 * 投票后 bridge 会把网页上揭示出来的真实模型名一并返回。
 */

import type { SlashCommand, CommandContext } from './types.js';
import { CommandKind } from './types.js';
import { setArenaChatMode, setArenaSideModels } from '../arena-mode.js';

const BRIDGE = process.env['ARENA_BRIDGE'] ?? 'http://127.0.0.1:17892';
// 强模型 + 长 prompt 常常要跑几分钟，5 分钟不够
const BATTLE_TIMEOUT_MS = 900000;

interface BattlePane {
  label?: string;
  text?: string;
}

interface BattleRound {
  ok?: boolean;
  error?: string;
  a?: BattlePane;
  b?: BattlePane;
}

interface VoteRound extends BattleRound {
  choice?: string;
}

async function bridgePost<T>(
  path: string,
  body: unknown,
  timeoutMs = 30000,
): Promise<T> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const res = await fetch(BRIDGE + path, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
      signal: controller.signal,
    });
    const text = await res.text();
    let data: Record<string, unknown>;
    try {
      data = JSON.parse(text) as Record<string, unknown>;
    } catch {
      throw new Error(text.slice(0, 200) || 'HTTP ' + res.status);
    }
    if (!res.ok || data['ok'] === false) {
      throw new Error(String(data['error'] ?? 'HTTP ' + res.status));
    }
    return data as T;
  } finally {
    clearTimeout(timer);
  }
}

/** bridge 把「网页要求人机验证」报成 NEED_VERIFY，直接抛给用户看不懂 */
function errText(e: unknown): string {
  const msg = e instanceof Error ? e.message : String(e);
  if (/NEED_VERIFY/.test(msg)) {
    return 'arena 要求人机验证：请在电脑的 Chrome 里完成验证，然后重试。';
  }
  return msg;
}

function sessionOf(context: CommandContext): string {
  return context.services.config?.getSessionId() ?? 'main';
}

/** 页面上「Thought for 5 seconds」这种思考提示不属于答案正文 */
function cleanPaneText(text: string): string {
  return text
    .replace(/^Thought for [^\n]*\n+/i, '')
    .replace(/^思考了[^\n]*\n+/i, '')
    .trim();
}

function paneBlock(pane: BattlePane | undefined, fallback: string): string {
  const label = (pane?.label || fallback).trim();
  const text = cleanPaneText((pane?.text || '').trim());
  return '**' + label + '**\n\n' + text;
}

function formatRound(round: BattleRound, title: string, hint: string): string {
  return [
    '**' + title + '**',
    '',
    paneBlock(round.a, 'A'),
    '',
    '---',
    '',
    paneBlock(round.b, 'B'),
    '',
    hint,
  ].join('\n');
}

function parseSideArgs(args: string): { models: string[]; prompt: string } {
  const m = args.match(/^\s*--models\s+(\S+)(?:\s+([\s\S]*))?$/);
  if (!m) return { models: [], prompt: args.trim() };
  return {
    models: (m[1] ?? '')
      .split(',')
      .map((s) => s.trim())
      .filter(Boolean)
      .slice(0, 2),
    prompt: (m[2] ?? '').trim(),
  };
}

const VOTE_ALIASES: Record<string, 'a' | 'b' | 'tie' | 'both'> = {
  a: 'a',
  '1': 'a',
  a更好: 'a',
  b: 'b',
  '2': 'b',
  b更好: 'b',
  tie: 'tie',
  平: 'tie',
  都好: 'tie',
  both: 'both',
  都不好: 'both',
};

const VOTE_LABELS: Record<'a' | 'b' | 'tie' | 'both', string> = {
  a: 'A 更好',
  b: 'B 更好',
  tie: '都好',
  both: '都不好',
};

function normalizeVote(arg: string): 'a' | 'b' | 'tie' | 'both' | null {
  return VOTE_ALIASES[arg.trim().toLowerCase()] ?? null;
}

export const battleCommand: SlashCommand = {
  name: 'battle',
  kind: CommandKind.BUILT_IN,
  supportedModes: ['interactive', 'non_interactive', 'acp'] as const,
  description: 'Battle：两个匿名模型回答同一个问题，投票后揭示模型名',
  action: async (context, args) => {
    const prompt = args.trim();
    if (!prompt) {
      setArenaChatMode('battle');
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: [
          '已切到 Battle 模式：之后每次提问都让两个匿名模型同时作答。',
          '每轮用 /vote a | b | tie | both 投票并揭示模型名。',
          '带问题直接跑一轮：/battle <问题>；切回：/agent。',
        ].join('\n'),
      };
    }
    try {
      const round = await bridgePost<BattleRound>(
        '/battle',
        { session: sessionOf(context), prompt, mode: 'battle' },
        BATTLE_TIMEOUT_MS,
      );
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: formatRound(
          round,
          'Battle',
          '投票：/vote a | b | tie | both（a/b 更好、tie 都好、both 都不好）',
        ),
      };
    } catch (e) {
      return {
        type: 'message' as const,
        messageType: 'error' as const,
        content: 'battle: ' + errText(e),
      };
    }
  },
};

export const sideCommand: SlashCommand = {
  name: 'side',
  kind: CommandKind.BUILT_IN,
  supportedModes: ['interactive', 'non_interactive', 'acp'] as const,
  description: 'Side by Side：指定两个模型并排回答同一个问题',
  action: async (context, args) => {
    const { models, prompt } = parseSideArgs(args);
    if (!prompt) {
      if (models.length) setArenaSideModels(models);
      setArenaChatMode('side');
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: [
          '已切到 Side by Side 模式：之后每次提问都让两个模型并排作答。',
          models.length
            ? '模型：' + models.join(' vs ')
            : '模型：沿用网页当前选中的两个（可用 /side --models a,b 指定）',
          '每轮用 /vote a | b | tie | both 投票。切回：/agent。',
        ].join('\n'),
      };
    }
    try {
      const round = await bridgePost<BattleRound>(
        '/battle',
        { session: sessionOf(context), prompt, mode: 'side', models },
        BATTLE_TIMEOUT_MS,
      );
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: formatRound(
          round,
          'Side by Side',
          '投票：/vote a | b | tie | both（a/b 更好、tie 都好、both 都不好）',
        ),
      };
    } catch (e) {
      return {
        type: 'message' as const,
        messageType: 'error' as const,
        content: 'side: ' + errText(e),
      };
    }
  },
};

export const voteCommand: SlashCommand = {
  name: 'vote',
  kind: CommandKind.BUILT_IN,
  supportedModes: ['interactive', 'non_interactive', 'acp'] as const,
  description: '给上一轮 Battle / Side by Side 投票并揭示模型名',
  action: async (context, args) => {
    const choice = normalizeVote(args);
    if (!choice) {
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: '用法：/vote a | b | tie | both',
      };
    }
    try {
      const round = await bridgePost<VoteRound>(
        '/vote',
        { session: sessionOf(context), choice },
        120000,
      );
      const a = round.a?.label || 'A';
      const b = round.b?.label || 'B';
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: [
          '投票：' + VOTE_LABELS[choice],
          '',
          'A = ' + a,
          'B = ' + b,
        ].join('\n'),
      };
    } catch (e) {
      return {
        type: 'message' as const,
        messageType: 'error' as const,
        content: 'vote: ' + errText(e),
      };
    }
  },
};

export const directCommand: SlashCommand = {
  name: 'direct',
  kind: CommandKind.BUILT_IN,
  supportedModes: ['interactive'] as const,
  description: '切到 Direct 模式：单模型直聊，不经过 agent、不带仓库',
  action: async (_context, args) => {
    setArenaChatMode('direct');
    const prompt = args.trim();
    if (!prompt) {
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content:
          '已切到 Direct 模式。之后的提问直接走 Arena 网页的 Direct 对话，不带仓库和 agent 工具。用 /agent 切回。',
      };
    }
    return { type: 'submit_prompt' as const, content: prompt };
  },
};

export const agentCommand: SlashCommand = {
  name: 'agent',
  kind: CommandKind.BUILT_IN,
  supportedModes: ['interactive'] as const,
  description: '切回 Agent 模式：带仓库、带 agent 工具',
  action: async (_context, args) => {
    setArenaChatMode('agent');
    const prompt = args.trim();
    if (!prompt) {
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: '已切回 Agent 模式。',
      };
    }
    return { type: 'submit_prompt' as const, content: prompt };
  },
};
