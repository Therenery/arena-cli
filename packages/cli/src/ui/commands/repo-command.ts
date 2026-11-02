/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type { SlashCommand } from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';
import { setArenaRepo } from '../arena-mode.js';

const BRIDGE = process.env['ARENA_BRIDGE'] ?? 'http://127.0.0.1:17892';

async function bridgeGet(path: string): Promise<Record<string, unknown>> {
  const res = await fetch(BRIDGE + path);
  const text = await res.text();
  let data: Record<string, unknown>;
  try {
    data = JSON.parse(text) as Record<string, unknown>;
  } catch {
    throw new Error(text.slice(0, 200) || `HTTP ${res.status}`);
  }
  if (!res.ok) {
    throw new Error(String(data['error'] ?? `HTTP ${res.status}`));
  }
  return data;
}

function repoList(data: Record<string, unknown>): string[] {
  return Array.isArray(data['repos']) ? (data['repos'] as string[]) : [];
}

export const repoCommand: SlashCommand = {
  name: 'repo',
  get description() {
    return t('list or switch the Arena workspace repository');
  },
  kind: CommandKind.BUILT_IN,
  supportedModes: ['interactive', 'non_interactive', 'acp'] as const,
  completion: async (context, partialArg) => {
    try {
      const data = await bridgeGet(
        '/repos?cached=1&session=' +
          encodeURIComponent(context.services.config?.getSessionId() ?? 'main'),
      );
      const q = (partialArg ?? '').trim().toLowerCase();
      return repoList(data).filter((r) => !q || r.toLowerCase().includes(q));
    } catch {
      return [];
    }
  },
  action: async (context) => {
    const arg = (context.invocation?.args ?? '').trim();
    const session = encodeURIComponent(
      context.services.config?.getSessionId() ?? 'main',
    );
    try {
      const data = await bridgeGet(
        arg
          ? '/repo?session=' + session + '&name=' + encodeURIComponent(arg)
          : '/repos?session=' + session,
      );
      setArenaRepo(String(data['repo'] ?? ''));
      if (arg) {
        return {
          type: 'message' as const,
          messageType: 'info' as const,
          content: `Repository: ${String(data['repo'] ?? arg)}`,
        };
      }
      const repos = repoList(data);
      const lines = [`Repository: ${String(data['repo'] ?? '(none)')}`, ''];
      lines.push(
        ...(repos.length
          ? repos.map((r) => `  ${r}`)
          : ['  (no repositories)']),
      );
      lines.push('', 'Switch with: /repo owner/name');
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: lines.join('\n'),
      };
    } catch (e) {
      return {
        type: 'message' as const,
        messageType: 'error' as const,
        content: `repo: ${(e as Error).message}`,
      };
    }
  },
};
