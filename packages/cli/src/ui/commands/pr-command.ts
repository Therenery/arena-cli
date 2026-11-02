/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { execFile } from 'node:child_process';
import type { SlashCommand } from './types.js';
import { CommandKind } from './types.js';
import { t } from '../../i18n/index.js';

const BRIDGE = process.env['ARENA_BRIDGE'] ?? 'http://127.0.0.1:17892';

interface PrSummary {
  number?: number;
  title?: string;
  state?: string;
  isDraft?: boolean;
  url?: string;
  headRefName?: string;
  baseRefName?: string;
  additions?: number;
  deletions?: number;
  changedFiles?: number;
  reviewDecision?: string;
}

async function bridgeGet(path: string): Promise<Record<string, unknown>> {
  const res = await fetch(BRIDGE + path);
  const text = await res.text();
  let data: Record<string, unknown>;
  try {
    data = JSON.parse(text) as Record<string, unknown>;
  } catch {
    throw new Error(text.slice(0, 200) || 'HTTP ' + res.status);
  }
  if (!res.ok) {
    throw new Error(String(data['error'] ?? 'HTTP ' + res.status));
  }
  return data;
}

/** Opens a URL in the user's browser; best effort, the URL is printed too. */
function openUrl(url: string): void {
  const done = () => {};
  try {
    if (process.platform === 'win32') {
      execFile('cmd', ['/c', 'start', '', url], done);
    } else if (process.platform === 'darwin') {
      execFile('open', [url], done);
    } else {
      execFile('xdg-open', [url], done);
    }
  } catch {
    // Best effort only.
  }
}

function prLine(pr: PrSummary): string {
  const state = pr.state ?? '';
  const draft = pr.isDraft ? ' [draft]' : '';
  return '  #' + pr.number + ' ' + state + draft + ' ' + (pr.title ?? '');
}

export const prCommand: SlashCommand = {
  name: 'pr',
  get description() {
    return t('list or open a pull request of the Arena workspace repository');
  },
  kind: CommandKind.BUILT_IN,
  supportedModes: ['interactive', 'non_interactive', 'acp'] as const,
  action: async (context) => {
    const arg = (context.invocation?.args ?? '').trim().replace(/^#/, '');
    try {
      if (!arg) {
        const data = await bridgeGet('/pr');
        const repo = String(data['repo'] ?? '');
        const prs = Array.isArray(data['prs'])
          ? (data['prs'] as PrSummary[])
          : [];
        const lines = [
          t('Pull requests') + ': ' + (repo || t('(no repository)')),
          '',
        ];
        if (prs.length) {
          lines.push(...prs.map(prLine));
        } else {
          lines.push('  ' + t('(no pull requests)'));
        }
        lines.push('', t('Open one with: /pr <number>'));
        return {
          type: 'message' as const,
          messageType: 'info' as const,
          content: lines.join('\n'),
        };
      }
      if (!/^[0-9]+$/.test(arg)) {
        return {
          type: 'message' as const,
          messageType: 'error' as const,
          content: t('pr: expected a pull request number, for example /pr 12'),
        };
      }
      const data = await bridgeGet('/pr?n=' + encodeURIComponent(arg));
      const pr = (data['pr'] ?? {}) as PrSummary;
      const url = String(pr.url ?? '');
      if (url) {
        openUrl(url);
      }
      const lines = [
        '#' +
          (pr.number ?? arg) +
          ' ' +
          (pr.state ?? '') +
          (pr.isDraft ? ' [draft]' : '') +
          ' ' +
          (pr.title ?? ''),
        '  ' + (pr.headRefName ?? '?') + ' -> ' + (pr.baseRefName ?? '?'),
        '  +' +
          (pr.additions ?? 0) +
          ' -' +
          (pr.deletions ?? 0) +
          ' / ' +
          (pr.changedFiles ?? 0) +
          ' files',
      ];
      if (pr.reviewDecision) {
        lines.push('  review: ' + pr.reviewDecision);
      }
      if (url) {
        lines.push('', url);
      }
      return {
        type: 'message' as const,
        messageType: 'info' as const,
        content: lines.join('\n'),
      };
    } catch (e) {
      return {
        type: 'message' as const,
        messageType: 'error' as const,
        content: 'pr: ' + (e as Error).message,
      };
    }
  },
};
