/**
 * @license
 * Copyright 2026 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/**
 * 当前 Arena 会话的界面状态：选中的仓库、对话模式（agent / direct）。
 *
 * 仓库由 /repo 写入，启动时用 bridge 的缓存填一次；模式由 /direct、/agent
 * 写入，同时落到 ARENA_CHAT_MODE，请求管线按它决定发给 arena-bridge 的
 * metadata.mode。
 */

import { useSyncExternalStore } from 'react';

export type ArenaChatMode = 'agent' | 'direct' | 'battle' | 'side';

export interface ArenaStatus {
  repo: string;
  mode: ArenaChatMode;
}

function normalizeMode(value: string | undefined): ArenaChatMode {
  return value === 'direct' || value === 'battle' || value === 'side'
    ? value
    : 'agent';
}

let state: ArenaStatus = {
  repo: '',
  mode: normalizeMode(process.env['ARENA_CHAT_MODE']),
};

const listeners = new Set<() => void>();

function emit(): void {
  for (const listener of listeners) listener();
}

export function getArenaStatus(): ArenaStatus {
  return state;
}

export function setArenaRepo(repo: string): void {
  const next = String(repo ?? '');
  if (state.repo === next) return;
  state = { ...state, repo: next };
  emit();
}

/** /side 选定的两个模型，随请求 metadata.models 发给 bridge。 */
export function setArenaSideModels(models: string[]): void {
  const value = models
    .map((m) => m.trim())
    .filter(Boolean)
    .slice(0, 2)
    .join(',');
  if (value) process.env['ARENA_SIDE_MODELS'] = value;
  else delete process.env['ARENA_SIDE_MODELS'];
}

export function setArenaChatMode(mode: ArenaChatMode): void {
  process.env['ARENA_CHAT_MODE'] = mode;
  if (state.mode === mode) return;
  state = { ...state, mode };
  emit();
}

let primed = false;
let pending = false;

/** 启动时从 bridge 缓存里读一次当前仓库，供页脚常驻显示。 */
export function primeArenaRepo(): void {
  if (primed || pending) return;
  if (process.env['NODE_ENV'] === 'test') return;
  pending = true;
  const base = process.env['ARENA_BRIDGE'] ?? 'http://127.0.0.1:17892';
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 1500);
  void fetch(base + '/repos?cached=1', { signal: controller.signal })
    .then((res) => (res.ok ? res.json() : null))
    .then((data: unknown) => {
      const repo = (data as { repo?: unknown } | null)?.repo;
      if (typeof repo === 'string' && repo) {
        setArenaRepo(repo);
        primed = true;
      }
    })
    .catch(() => {})
    .finally(() => {
      pending = false;
      clearTimeout(timer);
    });
}

/** 页脚用的模式标签；agent 模式不显示（它由仓库行代表）。 */
export function arenaModeLabel(mode: ArenaChatMode): string {
  if (mode === 'direct') return 'Arena: Direct (no agent)';
  if (mode === 'battle') return 'Arena: Battle';
  if (mode === 'side') return 'Arena: Side by Side';
  return '';
}

export function subscribeArenaStatus(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

export function useArenaStatus(): ArenaStatus {
  return useSyncExternalStore(
    subscribeArenaStatus,
    getArenaStatus,
    getArenaStatus,
  );
}
