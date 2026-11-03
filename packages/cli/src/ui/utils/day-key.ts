/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

/** Keys an instant by its local calendar day as `YYYY-MM-DD`. */
export function dayKey(date: Date): string {
  const y = date.getFullYear();
  const m = String(date.getMonth() + 1).padStart(2, '0');
  const d = String(date.getDate()).padStart(2, '0');
  return `${y}-${m}-${d}`;
}

/**
 * Parses a `YYYY-MM-DD` day key to local midnight of that day. Local
 * construction (`new Date(y, m-1, d)`) rather than `new Date(key)`: the
 * latter parses as UTC midnight, which a subsequent local normalization
 * shifts to the previous day in negative-offset timezones.
 */
export function parseDayKey(key: string): Date {
  const [y, m, d] = key.split('-').map(Number);
  return new Date(y, m - 1, d);
}
