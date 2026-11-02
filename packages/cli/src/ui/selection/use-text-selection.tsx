/**
 * @license
 * Copyright 2025 Qwen
 * SPDX-License-Identifier: Apache-2.0
 */

import { useCallback, useEffect, useRef, type MutableRefObject } from 'react';
import {
  useStdout,
  type FrameBoundary,
  type FrameCell,
  type ReadonlyFrame,
} from 'ink';
import { createDebugLogger } from '@qwen-code/qwen-code-core';
import { useMouseEvents } from '../hooks/useMouseEvents.js';
import type { MouseEvent } from '../utils/mouse.js';
import { copyToClipboard } from '../utils/commandUtils.js';
import { getScreenBuffer, type ScreenBuffer } from './screen-buffer.js';
import {
  SelectionState,
  type NormalizedSelection,
  type SelectionMode,
} from './selection-state.js';
import { getSelectedText } from './selection-text.js';
import { spanAtForMode } from './selection-span.js';
import {
  terminalToGrid,
  snapWideChar,
  pointInViewport,
  clampToViewport,
  type ViewportRect,
} from './selection-coords.js';

interface ScrollState {
  scrollTop: number;
  scrollHeight: number;
  innerHeight: number;
}

/**
 * One captured viewport row, keyed by its absolute content row (frame row
 * plus the scroll offset at capture time). Rows are kept verbatim so a
 * selection that spans more than one screen can be stitched into a synthetic
 * frame and read by the same extractor the visible selection uses.
 */
interface RowCapture {
  cells: readonly FrameCell[];
  boundaries: ReadonlyArray<FrameBoundary | null>;
}

/** How often a pointer held past an edge scrolls the transcript. */
const EDGE_SCROLL_INTERVAL_MS = 60;
/** Upper bound on captured rows per drag, so a long scroll stays cheap. */
const MAX_CAPTURED_ROWS = 4000;

const sameViewportContent = (
  previous: ReadonlyFrame | null,
  current: ReadonlyFrame,
  rect: ViewportRect,
): boolean => {
  if (!previous) {
    return false;
  }
  for (let y = rect.y; y < rect.y + rect.height; y++) {
    for (let x = rect.x; x < rect.x + rect.width; x++) {
      const cell = previous.cells[y]?.[x];
      const currentCell = current.cells[y]?.[x];
      if (
        cell?.value !== currentCell?.value ||
        cell?.fullWidth !== currentCell?.fullWidth
      ) {
        return false;
      }
    }
  }
  return true;
};

const sameViewportRect = (
  previous: ViewportRect | null,
  current: ViewportRect | null,
): current is ViewportRect =>
  previous !== null &&
  current !== null &&
  previous.x === current.x &&
  previous.y === current.y &&
  previous.width === current.width &&
  previous.height === current.height;

/**
 * Read-only view of the controller's live selection, for consumers that must
 * not own the state (the context-menu "Copy Selection" item). The ref is
 * populated on mount and cleared on unmount; `null` while no controller is
 * mounted.
 */
export interface SelectionQuery {
  /** Reading-order range of the current selection, or null when none. */
  getRange: () => NormalizedSelection | null;
}

export interface TextSelectionControllerProps {
  /** Selection is only handled while active (VP mode, no dialog, focused). */
  isActive: boolean;
  /**
   * Temporarily ignore mouse events WITHOUT clearing the current selection.
   * Used while the context menu owns the pointer: the existing selection must
   * survive (the menu's "Copy Selection" offers it), but new presses must not
   * start or extend a selection underneath the menu. Deactivation
   * (`isActive`) still clears; pausing does not.
   */
  eventsPaused?: boolean;
  /** Reads from the history viewport; called at event time (may be null early). */
  getViewportRect: () => ViewportRect | null;
  /** Additional selectable regions outside the history viewport. */
  getAdditionalSelectableRects?: () => readonly ViewportRect[];
  getScrollState: () => ScrollState;
  /**
   * Scrolls the history viewport by delta rows (positive scrolls towards
   * newer content). When provided, holding a drag past the top or bottom
   * edge of the viewport scrolls it and extends the selection, as a browser
   * does.
   */
  scrollBy?: (delta: number) => void;
  hitTestScrollbar: (location: { col: number; row: number }) => boolean;
  /** Optional sink exposing the live selection range to other components. */
  selectionQueryRef?: MutableRefObject<SelectionQuery | null>;
}

/**
 * Max gap between clicks (ms) to count as a double/triple click. Shared with
 * ContentMouseController, whose delayed link-open window must match this
 * value so a second press always lands inside it and cancels the open.
 */
export const MULTI_CLICK_MS = 400;
const debugLogger = createDebugLogger('TEXT_SELECTION');

interface ClickRecord {
  x: number;
  y: number;
  time: number;
  count: number;
}

/**
 * Headless controller that turns mouse press/drag/release in selectable VP
 * regions into a text selection: it maps terminal coordinates to the
 * composited frame, drives the {@link SelectionState}, highlights the range
 * through the frame controller, and copies on release. Double/triple click
 * select a word/line. B1 scope: visible-region only; history selections clear
 * on scroll, while every selection clears when its owning content or layout
 * changes.
 */
export function TextSelectionController(
  props: TextSelectionControllerProps,
): null {
  const { stdout } = useStdout();
  const selectionRef = useRef(new SelectionState());
  const dragScrollTopRef = useRef<number | null>(null);
  const anchorContentRowRef = useRef<number | null>(null);
  const edgeDirRef = useRef<0 | -1 | 1>(0);
  const edgeTimerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const edgeXRef = useRef<number>(0);
  const captureRef = useRef<Map<number, RowCapture>>(new Map());
  const baselineScrollTopRef = useRef<number>(0);
  const baselineScrollHeightRef = useRef<number>(0);
  const baselineFrameRef = useRef<ReadonlyFrame | null>(null);
  const baselineRectRef = useRef<ViewportRect | null>(null);
  const activeRectIndexRef = useRef<number | null>(null);
  const selectionGenerationRef = useRef(0);
  // Generation of a queued frame-invalidation that has not run yet; a release
  // in the same generation must not copy content the repaint already replaced.
  const pendingInvalidationGenRef = useRef<number | null>(null);
  const lastClickRef = useRef<ClickRecord | null>(null);
  // Anchor span and mode of an active word/line drag; null for char drags.
  const anchorSpanRef = useRef<{
    span: NormalizedSelection;
    mode: Exclude<SelectionMode, 'char'>;
  } | null>(null);
  const bufferRef = useRef<ScreenBuffer | undefined>(undefined);
  const propsRef = useRef(props);
  propsRef.current = props;

  const getBuffer = useCallback((): ScreenBuffer | undefined => {
    if (!bufferRef.current) {
      bufferRef.current = getScreenBuffer(stdout);
    }
    return bufferRef.current;
  }, [stdout]);

  const clearSelection = useCallback(() => {
    edgeDirRef.current = 0;
    if (edgeTimerRef.current !== null) {
      clearInterval(edgeTimerRef.current);
      edgeTimerRef.current = null;
    }
    anchorContentRowRef.current = null;
    captureRef.current.clear();
    selectionGenerationRef.current += 1;
    activeRectIndexRef.current = null;
    const selection = selectionRef.current;
    anchorSpanRef.current = null;
    if (selection.isEmpty) {
      return;
    }
    selection.clear();
    getBuffer()?.setSelection(null);
  }, [getBuffer]);

  const applyHighlight = useCallback(() => {
    const selection = selectionRef.current;
    const normalized = selection.normalized();
    if (!normalized || selection.isBareClick) {
      getBuffer()?.setSelection(null);
      return;
    }
    // Mirror getSelectableRects() without depending on its definition
    // order: index 0 is the history viewport, the rest are footer rects.
    const rectIndex = activeRectIndexRef.current;
    const rect =
      rectIndex === 0
        ? propsRef.current.getViewportRect()
        : rectIndex === null
          ? null
          : ((propsRef.current.getAdditionalSelectableRects?.() ?? [])[
              rectIndex - 1
            ] ?? null);
    if (!rect) {
      getBuffer()?.setSelection(normalized);
      return;
    }
    // An edge-scrolled anchor can sit above the viewport; paint the part of
    // the range that is on screen instead of a coordinate outside the frame.
    let sx = normalized.sx;
    let sy = normalized.sy;
    let ex = normalized.ex;
    let ey = normalized.ey;
    if (sy < rect.y) {
      sy = rect.y;
      sx = rect.x;
    }
    if (ey > rect.y + rect.height - 1) {
      ey = rect.y + rect.height - 1;
      ex = rect.x + rect.width - 1;
    }
    if (sy > ey) {
      getBuffer()?.setSelection(null);
      return;
    }
    getBuffer()?.setSelection({ sx, sy, ex, ey });
  }, [getBuffer]);

  const getSelectableRects = useCallback((): readonly ViewportRect[] => {
    const viewportRect = propsRef.current.getViewportRect();
    if (!viewportRect) {
      return [];
    }
    const additionalRects =
      propsRef.current.getAdditionalSelectableRects?.() ?? [];
    return [viewportRect, ...additionalRects];
  }, []);

  const getActiveRect = useCallback((): ViewportRect | null => {
    const index = activeRectIndexRef.current;
    return index === null ? null : (getSelectableRects()[index] ?? null);
  }, [getSelectableRects]);

  // Rows seen while a drag is in flight, keyed by absolute content row, so a
  // range that outlives the visible window can still be read back at copy
  // time.
  const captureRows = useCallback(
    (rect: ViewportRect, scrollTop: number) => {
      const frame = getBuffer()?.frame ?? null;
      if (!frame) return;
      const map = captureRef.current;
      for (let y = rect.y; y < rect.y + rect.height; y++) {
        const cells = frame.cells[y];
        if (!cells) continue;
        map.set(scrollTop + (y - rect.y), {
          cells,
          boundaries: frame.boundaries[y] ?? [],
        });
      }
      if (map.size > MAX_CAPTURED_ROWS) {
        const keys = [...map.keys()].sort((a, b) => a - b);
        for (const key of keys.slice(0, map.size - MAX_CAPTURED_ROWS)) {
          map.delete(key);
        }
      }
    },
    [getBuffer],
  );

  const stopEdgeScroll = useCallback(() => {
    edgeDirRef.current = 0;
    if (edgeTimerRef.current !== null) {
      clearInterval(edgeTimerRef.current);
      edgeTimerRef.current = null;
    }
  }, []);

  // Browser-style edge scrolling: while the pointer is held past the top or
  // bottom of the viewport, walk the transcript one row per tick. The frame
  // subscription shifts the live range by the same delta, so the selection
  // stays anchored to the text under the pointer.
  const startEdgeScroll = useCallback(
    (dir: -1 | 1) => {
      if (!propsRef.current.scrollBy) {
        return;
      }
      if (edgeDirRef.current === dir && edgeTimerRef.current !== null) {
        return;
      }
      stopEdgeScroll();
      edgeDirRef.current = dir;
      edgeTimerRef.current = setInterval(() => {
        if (edgeDirRef.current === 0) return;
        if (!getActiveRect()) return;
        const state = propsRef.current.getScrollState();
        const atEnd =
          edgeDirRef.current > 0
            ? state.scrollTop >= state.scrollHeight - state.innerHeight
            : state.scrollTop <= 0;
        if (atEnd) return;
        propsRef.current.scrollBy?.(edgeDirRef.current);
      }, EDGE_SCROLL_INTERVAL_MS);
    },
    [getActiveRect, stopEdgeScroll],
  );

  const recordBaseline = useCallback(
    (rect: ViewportRect) => {
      const scrollState = propsRef.current.getScrollState();
      baselineScrollTopRef.current = scrollState.scrollTop;
      baselineScrollHeightRef.current = scrollState.scrollHeight;
      baselineFrameRef.current = getBuffer()?.frame ?? null;
      baselineRectRef.current = rect;
    },
    [getBuffer],
  );

  /**
   * Text for a range whose anchor has scrolled out of the visible frame.
   * Rows captured during the drag are stitched into a synthetic frame so the
   * normal extractor still applies soft-wrap joiners and wide-glyph rules.
   * Returns null when the whole range is on screen.
   */
  const stitchedText = useCallback(
    (selection: SelectionState): string | null => {
      // Only history-viewport selections are captured; footer ranges live
      // outside the scrollable region and are always read from the frame.
      if (activeRectIndexRef.current !== 0) {
        return null;
      }
      const anchorRow = anchorContentRowRef.current;
      if (anchorRow === null || !selection.anchor || !selection.focus) {
        return null;
      }
      const { scrollTop } = propsRef.current.getScrollState();
      const focusRow = selection.focus.y + scrollTop;
      const from = Math.min(anchorRow, focusRow);
      const to = Math.max(anchorRow, focusRow);
      const rect = propsRef.current.getViewportRect();
      if (rect && from >= scrollTop && to <= scrollTop + rect.height - 1) {
        return null;
      }
      const capture = captureRef.current;
      const rows: RowCapture[] = [];
      for (let row = from; row <= to; row++) {
        rows.push(capture.get(row) ?? { cells: [], boundaries: [] });
      }
      if (!rows.length) {
        return null;
      }
      const frame: ReadonlyFrame = {
        width: getBuffer()?.frame?.width ?? 0,
        height: rows.length,
        cells: rows.map((row) => row.cells),
        boundaries: rows.map((row) => row.boundaries),
      };
      const sx = anchorRow <= focusRow ? selection.anchor.x : selection.focus.x;
      const ex = anchorRow <= focusRow ? selection.focus.x : selection.anchor.x;
      return getSelectedText(frame, { sx, sy: 0, ex, ey: rows.length - 1 });
    },
    [getBuffer],
  );

  const copySelection = useCallback(() => {
    const selection = selectionRef.current;
    const normalized = selection.normalized();
    const stitched = normalized ? stitchedText(selection) : null;
    const text =
      stitched !== null
        ? stitched
        : normalized
          ? getSelectedText(getBuffer()?.frame ?? null, normalized)
          : '';
    if (text) {
      void copyToClipboard(text).catch((error: unknown) => {
        debugLogger.warn('Failed to copy selected text:', error);
      });
    }
  }, [getBuffer, stitchedText]);

  const mapEvent = useCallback(
    (event: MouseEvent): ReturnType<typeof terminalToGrid> | null => {
      const buffer = getBuffer();
      if (!buffer) {
        return null;
      }
      const frameHeight = buffer.dimensions.height;
      const terminalHeight =
        (stdout as unknown as { rows?: number }).rows ?? frameHeight;
      const point = terminalToGrid(
        event.col,
        event.row,
        terminalHeight,
        frameHeight,
      );
      return snapWideChar(buffer.frame, point);
    },
    [getBuffer, stdout],
  );

  // Extend an active word/line drag so the range spans from the original
  // multi-click span to the word/line boundary at the current point (issue
  // #8738). Falls back to a single cell when the cursor is over whitespace.
  const extendSpanDrag = useCallback(
    (point: { x: number; y: number }) => {
      const anchor = anchorSpanRef.current;
      if (!anchor) return;
      const { span, mode } = anchor;
      const selection = selectionRef.current;
      const frame = getBuffer()?.frame ?? null;
      const current = spanAtForMode(frame, mode, point) ?? {
        sx: point.x,
        sy: point.y,
        ex: point.x,
        ey: point.y,
      };
      const cursorAfter =
        point.y > span.ey || (point.y === span.ey && point.x >= span.ex);
      selection.anchor = cursorAfter
        ? { x: span.sx, y: span.sy }
        : { x: span.ex, y: span.ey };
      selection.focus = cursorAfter
        ? { x: current.ex, y: current.ey }
        : { x: current.sx, y: current.sy };
    },
    [getBuffer],
  );

  const extendActiveDrag = useCallback(
    (point: { x: number; y: number }) => {
      if (anchorSpanRef.current) {
        extendSpanDrag(point);
      } else {
        selectionRef.current.extend(point);
      }
    },
    [extendSpanDrag],
  );

  const handleMouse = useCallback(
    (event: MouseEvent) => {
      const selection = selectionRef.current;

      // While paused (context menu owns the pointer) ignore press/move/release
      // so they can't start or extend a selection under the menu — but do NOT
      // clear: the existing selection is what the menu's Copy Selection
      // offers. Nothing can scroll the viewport while paused, so wheel ticks
      // are dropped here too (before the scroll-clear branch). A left-release
      // must still finish a drag that was in flight when the menu opened, or
      // the stale dragging state misroutes the next press/release pair.
      if (propsRef.current.eventsPaused) {
        if (event.name === 'left-release') {
          selection.finish();
        }
        return;
      }

      // History scrolls invalidate history-owned selection coordinates. Footer
      // selections live outside the scrollable viewport and remain valid.
      if (event.name.startsWith('scroll-')) {
        if (activeRectIndexRef.current === 0) {
          clearSelection();
        }
        return;
      }

      if (event.name === 'left-press') {
        selectionGenerationRef.current += 1;
        if (
          propsRef.current.hitTestScrollbar({ col: event.col, row: event.row })
        ) {
          clearSelection();
          return;
        }
        const point = mapEvent(event);
        const rects = getSelectableRects();
        const rectIndex = point
          ? rects.findIndex((rect) => pointInViewport(point, rect))
          : -1;
        if (!point || rectIndex < 0) {
          clearSelection();
          return;
        }
        const rect = rects[rectIndex];
        activeRectIndexRef.current = rectIndex;

        // Multi-click detection (double = word, triple = line).
        const now = Date.now();
        const prev = lastClickRef.current;
        const near =
          prev != null &&
          prev.y === point.y &&
          Math.abs(prev.x - point.x) <= 1 &&
          now - prev.time < MULTI_CLICK_MS;
        const count = near ? Math.min(prev!.count + 1, 3) : 1;
        lastClickRef.current = { x: point.x, y: point.y, time: now, count };

        if (count >= 2) {
          const frame = getBuffer()?.frame ?? null;
          const mode = count === 2 ? 'word' : 'line';
          const span = spanAtForMode(frame, mode, point);
          if (span) {
            // Enter a drag-capable word/line selection so a held double/triple
            // click can extend by word/line on move (issue #8738). Copy at
            // press too: a streaming repaint before release clears the
            // selection, skipping the release copy. Release copies again when
            // the drag grew the range.
            selection.start({ x: span.sx, y: span.sy }, mode);
            selection.extend({ x: span.ex, y: span.ey });
            anchorSpanRef.current = { span, mode };
            stopEdgeScroll();
            dragScrollTopRef.current =
              propsRef.current.getScrollState().scrollTop;
            anchorContentRowRef.current = point.y + dragScrollTopRef.current;
            captureRows(rect, dragScrollTopRef.current);
            recordBaseline(rect);
            applyHighlight();
            copySelection();
            return;
          }
        }

        anchorSpanRef.current = null;
        selection.start(point);
        stopEdgeScroll();
        dragScrollTopRef.current = propsRef.current.getScrollState().scrollTop;
        anchorContentRowRef.current = point.y + dragScrollTopRef.current;
        captureRows(rect, dragScrollTopRef.current);
        recordBaseline(rect);
        applyHighlight();
        return;
      }

      if (event.name === 'move') {
        if (!selection.dragging) {
          return;
        }
        // A held multi-click keeps its click record so pointer drift cannot
        // break a triple-click; char drags still break the chain.
        if (!anchorSpanRef.current) {
          lastClickRef.current = null;
        }
        // A scroll under a history drag invalidates its viewport coordinates.
        if (
          edgeDirRef.current === 0 &&
          activeRectIndexRef.current === 0 &&
          propsRef.current.getScrollState().scrollTop !==
            dragScrollTopRef.current
        ) {
          clearSelection();
          return;
        }
        const point = mapEvent(event);
        const rect = getActiveRect();
        if (!point || !rect) {
          clearSelection();
          return;
        }
        // Holding the drag past an edge scrolls the transcript, the way a
        // browser keeps extending a selection past the window.
        edgeXRef.current = Math.max(
          rect.x,
          Math.min(rect.x + rect.width - 1, point.x),
        );
        if (point.y < rect.y) {
          startEdgeScroll(-1);
        } else if (point.y >= rect.y + rect.height) {
          startEdgeScroll(1);
        } else {
          stopEdgeScroll();
        }
        captureRows(rect, propsRef.current.getScrollState().scrollTop);
        extendActiveDrag(clampToViewport(point, rect));
        applyHighlight();
        return;
      }

      if (event.name === 'left-release') {
        if (!selection.dragging) {
          return;
        }
        stopEdgeScroll();
        // A repaint invalidated this selection but the deferred clear has not
        // run yet; drop it without extending or copying the stale content.
        if (
          pendingInvalidationGenRef.current === selectionGenerationRef.current
        ) {
          clearSelection();
          return;
        }
        const point = mapEvent(event);
        const rect = getActiveRect();
        if (point && rect) {
          extendActiveDrag(clampToViewport(point, rect));
        }
        // Clear the span drag only after extending, so the release cell still
        // applies to a word/line drag when no move covered it.
        anchorSpanRef.current = null;
        selection.finish();
        if (selection.isEmpty || selection.isBareClick) {
          clearSelection();
          return;
        }
        applyHighlight();
        copySelection();
        return;
      }
    },
    [
      clearSelection,
      applyHighlight,
      copySelection,
      recordBaseline,
      mapEvent,
      getBuffer,
      getSelectableRects,
      getActiveRect,
      extendActiveDrag,
      captureRows,
      startEdgeScroll,
      stopEdgeScroll,
    ],
  );

  useMouseEvents(handleMouse, {
    isActive: props.isActive,
    tracking: 'button',
  });

  // Invalidate the selection when the content scrolls, streams, or the terminal
  // resizes — anything that moves the composited frame under a fixed selection.
  // A resize reflows content, which changes the frame/scroll height, so the
  // frame subscription already covers it (no extra stdout 'resize' listener,
  // which would trip the max-listeners warning). Our own highlight renders keep
  // the frame's characters and dimensions unchanged, so this does not feed back
  // into a render loop.
  useEffect(() => {
    const buffer = getBuffer();
    if (!buffer) {
      return;
    }
    return buffer.subscribe((frame) => {
      if (selectionRef.current.isEmpty) {
        return;
      }
      const { scrollTop, scrollHeight } = propsRef.current.getScrollState();
      const activeRect = getActiveRect();
      const isViewportSelection = activeRectIndexRef.current === 0;

      // Our own edge scroll moves the content under a live drag. Shift the
      // range by the same delta so it stays anchored to the text the user
      // started on, pin the focus to the edge being held, and re-baseline so
      // the invalidation below does not read this as an external scroll.
      const delta = scrollTop - baselineScrollTopRef.current;
      if (
        selectionRef.current.dragging &&
        edgeDirRef.current !== 0 &&
        isViewportSelection &&
        delta !== 0
      ) {
        const selection = selectionRef.current;
        if (selection.anchor) {
          selection.anchor = {
            x: selection.anchor.x,
            y: selection.anchor.y - delta,
          };
        }
        if (selection.focus) {
          selection.focus = {
            x: selection.focus.x,
            y: selection.focus.y - delta,
          };
        }
        const span = anchorSpanRef.current;
        if (span) {
          anchorSpanRef.current = {
            span: {
              sx: span.span.sx,
              sy: span.span.sy - delta,
              ex: span.span.ex,
              ey: span.span.ey - delta,
            },
            mode: span.mode,
          };
        }
        if (activeRect) {
          selection.focus = {
            x: Math.max(
              activeRect.x,
              Math.min(activeRect.x + activeRect.width - 1, edgeXRef.current),
            ),
            y:
              edgeDirRef.current > 0
                ? activeRect.y + activeRect.height - 1
                : activeRect.y,
          };
          captureRows(activeRect, scrollTop);
        }
        baselineScrollTopRef.current = scrollTop;
        baselineScrollHeightRef.current = scrollHeight;
        baselineFrameRef.current = frame;
        baselineRectRef.current = activeRect;
        dragScrollTopRef.current = scrollTop;
        applyHighlight();
        return;
      }

      if (
        (isViewportSelection &&
          (scrollTop !== baselineScrollTopRef.current ||
            scrollHeight !== baselineScrollHeightRef.current)) ||
        !sameViewportRect(baselineRectRef.current, activeRect) ||
        !sameViewportContent(baselineFrameRef.current, frame, activeRect)
      ) {
        const invalidatedGeneration = selectionGenerationRef.current;
        pendingInvalidationGenRef.current = invalidatedGeneration;
        queueMicrotask(() => {
          pendingInvalidationGenRef.current = null;
          if (selectionGenerationRef.current === invalidatedGeneration) {
            clearSelection();
          }
        });
      }
    });
  }, [getBuffer, getActiveRect, clearSelection, captureRows, applyHighlight]);

  useEffect(() => {
    if (!props.isActive) {
      clearSelection();
    }
  }, [props.isActive, clearSelection]);

  // The painted range lives on the per-stdout frame controller, outside React,
  // so it outlives this controller: ink re-reads it on every serialization pass
  // and stamps the highlight onto whatever cells now occupy those coordinates.
  // A view that unmounts with a selection on screen (switching away from a
  // teammate tab, or between teammates) would therefore leave a highlight band
  // over content the user never selected there. `clearSelection()` cannot do
  // this teardown: it early-returns while its own selection is empty, and a
  // successor controller always starts empty, so nothing downstream repairs it
  // either. Unmount only — never `eventsPaused`, which must keep the selection
  // the open context menu's "Copy Selection" offers. `getBuffer` is a
  // `useCallback` keyed on `[stdout]`, so this cleanup runs once, on unmount.
  useEffect(
    () => () => {
      stopEdgeScroll();
      getBuffer()?.setSelection(null);
    },
    [getBuffer, stopEdgeScroll],
  );

  // Expose the live selection to an external query ref (context menu "Copy
  // Selection"). A collapsed char-mode selection is a bare click (cleared on
  // release anyway) and carries no text; a collapsed word/line span still
  // holds its cell(s), so it stays offerable.
  const selectionQueryRef = props.selectionQueryRef;
  useEffect(() => {
    if (!selectionQueryRef) {
      return;
    }
    selectionQueryRef.current = {
      getRange: () => {
        const selection = selectionRef.current;
        if (
          selection.isEmpty ||
          (selection.isCollapsed && selection.mode === 'char')
        ) {
          return null;
        }
        return selection.normalized();
      },
    };
    return () => {
      selectionQueryRef.current = null;
    };
  }, [selectionQueryRef]);

  return null;
}
