/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type { SlashCommand } from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';

const BRIDGE = process.env['ARENA_BRIDGE'] ?? 'http://127.0.0.1:17892';

function parsePaths(input: string): string[] {
  const out: string[] = [];
  const re = /"([^"]+)"|'([^']+)'|(\S+)/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(input)) !== null) {
    out.push(m[1] ?? m[2] ?? m[3]);
  }
  return out;
}

export const uploadCommand: SlashCommand = {
  name: 'upload',
  get description() {
    return t('attach local files to the Arena composer');
  },
  kind: CommandKind.BUILT_IN,
  supportedModes: ['interactive', 'non_interactive', 'acp'] as const,
  action: async (context) => {
    const args = (context.invocation?.args ?? '').trim();
    if (!args) {
      return {
        type: 'message' as const,
        messageType: 'warning' as const,
        content: 'Usage: /upload <path> [path...]',
      };
    }
    const files = parsePaths(args);
    try {
      const res = await fetch(`${BRIDGE}/upload`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ session: 'main', files }),
      });
      const data = (await res.json()) as Record<string, unknown>;
      if (!res.ok) {
        throw new Error(String(data['error'] ?? `HTTP ${res.status}`));
      }
      const attached = Array.isArray(data['attached'])
        ? (data['attached'] as string[])
        : files;
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: `Attached: ${attached.join(', ')}`,
      };
    } catch (e) {
      return {
        type: 'message' as const,
        messageType: 'error' as const,
        content: `upload: ${(e as Error).message}`,
      };
    }
  },
};
