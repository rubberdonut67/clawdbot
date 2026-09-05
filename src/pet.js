// Canvas sprite engine for the buddy. Renders CLIPS from sprites.js with
// per-state motion and overlay glyphs. Host-agnostic: no Tauri APIs in here.
//
// Rendering: each frame is composited onto a native-resolution offscreen
// canvas, then drawn once at an integer number of *device* pixels per sprite
// cell — crisp at any Windows display scaling and any pet scale.
//
// Coordinates: all internal layout is in a fixed 160x160 "unit" space.
// The pet scale S multiplies units -> CSS px; devicePixelRatio multiplies
// CSS px -> device px. opaqueBounds() reports CSS px.
'use strict';

import { CLIPS, PALETTES, GLYPHS, SPRITE_W, SPRITE_H } from './sprites.js';

const CELL = 8;                  // units per sprite cell
const UNIT_W = 160;
const UNIT_H = 160;
const GROUND_Y = UNIT_H - 12;    // buddy feet rest here (units)
// Worst-case motion offsets (units): done's flag frame is 32 half-rows
// tall (top at unit 20; rest y is 68 -> up 48) and sways dx +4; working's
// away frames sit at dx -28 with the laptop art at frame col 0 = canvas
// unit 4 (left 28); soccer's 34-half-col frames at dx -20 reach 20 units
// past the rest edge on the right; droop dy +4.
const MOTION_ENVELOPE = { left: 28, right: 24, up: 48, down: 4 };

// Motion transforms return integer unit offsets.
const MOTIONS = {
  none:    () => ({ dx: 0, dy: 0 }),
  breathe: (t) => ({ dx: 0, dy: Math.sin(t * 1.6) > 0.3 ? 1 : 0 }),
  droop:   () => ({ dx: 0, dy: 4 }),
  tilt:    (t) => ({ dx: Math.round(Math.sin(t * 1.2) * 2), dy: 0 }),
  wobble:  (t) => ({ dx: Math.round(Math.sin(t * 2.2) * 2), dy: 0 }),
  work:    (t) => ({ dx: Math.round(Math.sin(t * 3)), dy: -Math.round(Math.abs(Math.sin(t * 8)) * 2) }),
  hopOnce: (t) => ({ dx: 0, dy: t < 0.55 ? -Math.round(Math.sin((t / 0.55) * Math.PI) * 8) : 0 }),
};

// Idle nap cadence: after a random 3-8 min of uninterrupted idle the buddy
// falls asleep; the nap ends on its own after 45s-2min. A click or drag
// wakes it early. Deliberately much rarer than the old soccer interlude.
const SLEEP_MIN_IDLE_MS = 180000;
const SLEEP_MAX_IDLE_MS = 480000;
const NAP_MIN_MS = 45000;
const NAP_MAX_MS = 120000;

export class Pet {
  constructor(canvas, scale = 1) {
    this.canvas = canvas;
    this.state = 'idle';
    this.detail = null;
    this.stateSince = performance.now();
    this.listeners = {};
    this._sleepTimer = null;
    this._wakeTimer = null;
    this._napMs = 0;
    this._armSleep(); // initial state is idle
    // error-state glitch: random tear-into-bands moments (see _raf)
    this._glitch = { nextAt: 0, until: 0, bands: [0, 0, 0], grey: false };

    this.off = document.createElement('canvas');
    this.off.width = SPRITE_W;
    this.off.height = SPRITE_H;
    this.offCtx = this.off.getContext('2d');
    // separate offscreen for the dream vignette: the main draw resizes
    // this.off per frame, so the mini scene gets its own compositor
    this.dreamOff = document.createElement('canvas');
    this.dreamCtx = this.dreamOff.getContext('2d');

    this.setScale(scale);

    this._raf = this._raf.bind(this);
    requestAnimationFrame(this._raf);
  }

  on(evt, fn) { (this.listeners[evt] = this.listeners[evt] || []).push(fn); }
  _emit(evt, arg) { (this.listeners[evt] || []).forEach((f) => f(arg)); }

  // Pet scale S: units -> CSS px. Reconfigures the canvas; integer device
  // cells keep pixels even at fractional display scaling times any S.
  setScale(scale) {
    this.scale = Math.min(3, Math.max(0.5, scale));
    this.dpr = window.devicePixelRatio || 1;
    this.k = this.scale * this.dpr;           // units -> device px
    this.cell = Math.max(1, Math.round(CELL * this.k));
    this.canvas.width = Math.round(UNIT_W * this.k);
    this.canvas.height = Math.round(UNIT_H * this.k);
    this.canvas.style.width = (UNIT_W * this.scale) + 'px';
    this.canvas.style.height = (UNIT_H * this.scale) + 'px';
    this.ctx = this.canvas.getContext('2d');
    this.ctx.imageSmoothingEnabled = false;
    this._emit('scale', this.scale);
  }

  setState(state, detail = null) {
    if (!CLIPS[state]) throw new Error('unknown state: ' + state);
    // While an outro is in flight, any request just re-targets its pending
    // state — including back to the outgoing state (a fast A→B→A flap must
    // end on A, so this check runs before the same-state short-circuit).
    if (this._outro) { this._outro.pending = { state, detail }; return; }
    if (state === this.state) { this.detail = detail; return; }
    // A clip with an outro finishes its exit move before the next state shows
    // (e.g. working: leap back home and land, per the reference GIF).
    const cur = CLIPS[this._clipName()];
    if (cur.stages && cur.stages.outro.length) {
      this._outro = { steps: cur.stages.outro, palette: cur.palette, res: cur.res || 1, overlay: cur.overlay, start: performance.now(), pending: { state, detail } };
      return;
    }
    this._apply(state, detail);
  }

  _apply(state, detail) {
    this.state = state;
    this.detail = detail;
    this.stateSince = performance.now();
    this._emit('state', { state, detail });
    // attention states always win: the nap timers only ever run in the two
    // calm states — arm sleep while idle, arm auto-wake while sleeping
    this._disarmSleep();
    this._disarmWake();
    if (state === 'idle') this._armSleep();
    else if (state === 'sleeping') this._armWake();
  }

  // --- idle nap cycle: a long stretch of idle drifts into sleep; the nap
  // ends on its own (or early, when the host wakes the pet on click/drag) ---
  _armSleep() {
    this._disarmSleep();
    const span = SLEEP_MAX_IDLE_MS - SLEEP_MIN_IDLE_MS;
    this._sleepTimer = setTimeout(() => this._startSleep(), SLEEP_MIN_IDLE_MS + Math.random() * span);
  }

  _disarmSleep() {
    if (this._sleepTimer) { clearTimeout(this._sleepTimer); this._sleepTimer = null; }
  }

  _startSleep() {
    this._sleepTimer = null;
    if (this.state !== 'idle' || this._outro) return;
    this.setState('sleeping');
  }

  _armWake() {
    this._disarmWake();
    const span = NAP_MAX_MS - NAP_MIN_MS;
    // the dream overlay paces its story to the real nap length, so the
    // mini buddy finishes its work exactly when the pet wakes
    this._napMs = NAP_MIN_MS + Math.random() * span;
    this._wakeTimer = setTimeout(() => this.setState('idle'), this._napMs);
  }

  _disarmWake() {
    if (this._wakeTimer) { clearTimeout(this._wakeTimer); this._wakeTimer = null; }
  }

  // Rest-position sprite rect in units (canvas-relative).
  restBounds() {
    const w = SPRITE_W * CELL, h = SPRITE_H * CELL;
    return { x: (UNIT_W - w) / 2, y: GROUND_Y - h, w, h };
  }

  // Clickable region for the Tauri hit test, in CSS px: rest rect expanded by
  // the worst-case motion envelope so the buddy stays clickable mid-hop.
  opaqueBounds() {
    const b = this.restBounds();
    const s = this.scale;
    return {
      x: (b.x - MOTION_ENVELOPE.left) * s,
      y: (b.y - MOTION_ENVELOPE.up) * s,
      w: (b.w + MOTION_ENVELOPE.left + MOTION_ENVELOPE.right) * s,
      h: (b.h + MOTION_ENVELOPE.up + MOTION_ENVELOPE.down) * s,
    };
  }

  // Is a CSS-px point (canvas-relative) over the clickable region?
  hitTest(cssX, cssY) {
    const b = this.opaqueBounds();
    return cssX >= b.x && cssX < b.x + b.w && cssY >= b.y && cssY < b.y + b.h;
  }

  _clipName() {
    return this.state;
  }

  _clipTime() { return (performance.now() - this.stateSince) / 1000; }

  // Resolve the frame + offset to draw right now, handling outro transitions
  // and staged clips (intro -> loop) alongside plain looping clips.
  _currentFrame() {
    const now = performance.now();
    if (this._outro) {
      let acc = 0;
      const el = now - this._outro.start;
      for (const s of this._outro.steps) {
        acc += s.ms;
        if (el < acc) return { frame: s.frame, fdx: s.dx, fdy: s.dy, palette: this._outro.palette, motion: 'none', overlay: this._outro.overlay, res: this._outro.res };
      }
      const p = this._outro.pending;
      this._outro = null;
      this._apply(p.state, p.detail);
      // fall through to render the new state this same frame
    }
    const clip = CLIPS[this._clipName()];
    const res = clip.res || 1;
    const t = this._clipTime();
    if (clip.stages) { // intro once, then loop cycles on measured durations
      const step = (s) => ({ frame: s.frame, fdx: s.dx, fdy: s.dy, palette: clip.palette, motion: clip.motion, overlay: clip.overlay, res });
      let ms = t * 1000;
      for (const s of clip.stages.intro) {
        if (ms < s.ms) return step(s);
        ms -= s.ms;
      }
      const loop = clip.stages.loop;
      ms %= loop.reduce((a, s) => a + s.ms, 0);
      for (const s of loop) {
        if (ms < s.ms) return step(s);
        ms -= s.ms;
      }
      return step(loop[loop.length - 1]); // float rounding fallback
    }
    const frame = clip.frames[Math.floor(t * clip.fps) % clip.frames.length];
    return { frame, fdx: 0, fdy: 0, palette: clip.palette, motion: clip.motion, overlay: clip.overlay, res };
  }

  _raf() {
    const cur = this._currentFrame();
    const t = this._clipTime();
    const ctx = this.ctx;
    ctx.clearRect(0, 0, this.canvas.width, this.canvas.height);

    // error-state glitch: every 1.5-5s the sprite tears into three offset
    // bands for ~100-150ms, occasionally flashing the grey palette —
    // digital distress, small offsets, never red (cuteness law)
    let glitch = null;
    if (this.state === 'error' && !this._outro) {
      const now = performance.now();
      const g = this._glitch;
      if (now > g.until && now > g.nextAt) {
        g.until = now + 100 + Math.random() * 50;
        g.nextAt = g.until + 1500 + Math.random() * 3500;
        const off = () => (Math.random() < 0.4 ? 0 : (Math.random() < 0.5 ? -1 : 1) * (1 + Math.round(Math.random())));
        g.bands = [off(), off(), off()];
        if (!g.bands.some((b) => b !== 0)) g.bands[1] = 1;
        g.grey = Math.random() < 0.3;
      }
      if (now < g.until) glitch = g;
    } else {
      this._glitch.until = 0;
      this._glitch.nextAt = 0;
    }

    const frame = cur.frame;
    const palette = PALETTES[glitch && glitch.grey ? 'grey' : cur.palette];

    const m = (MOTIONS[cur.motion] || MOTIONS.none)(t);
    const dx = m.dx + cur.fdx, dy = m.dy + cur.fdy;

    // composite the frame at its native resolution (res 2 = half-cell art)
    const rows = frame.length, cols = frame[0].length;
    if (this.off.width !== cols || this.off.height !== rows) {
      this.off.width = cols;
      this.off.height = rows;
    }
    this.offCtx.clearRect(0, 0, cols, rows);
    for (let r = 0; r < rows; r++) {
      const row = frame[r];
      for (let c = 0; c < cols; c++) {
        const ch = row[c];
        if (ch === '.') continue;
        this.offCtx.fillStyle = palette[ch] || '#ff00ff';
        this.offCtx.fillRect(c, r, 1, 1);
      }
    }

    // single scaled draw; cell size shrinks with frame resolution (the staged
    // clips use res 2 = half-cell art at 4 units per cell). The sprite is
    // anchored by its BOTTOM row to the ground so self-sized frames of any
    // height stand on the same line; for res-1 clips this equals restBounds().y.
    const res = cur.res || 1;
    const unitCell = CELL / res;
    const cellPx = Math.max(1, Math.round(unitCell * this.k));
    const base = this.restBounds();
    const ox = base.x + dx, oy = GROUND_Y - rows * unitCell + dy;
    const dev = (v) => Math.round(v * this.k);
    if (glitch) { // three horizontal slices, each shifted by its band offset
      const bandH = Math.ceil(rows / 3);
      for (let b = 0; b < 3; b++) {
        const sy = b * bandH;
        const h = Math.min(bandH, rows - sy);
        if (h <= 0) break;
        ctx.drawImage(this.off, 0, sy, cols, h,
          dev(ox + glitch.bands[b] * unitCell), dev(oy) + sy * cellPx, cols * cellPx, h * cellPx);
      }
    } else {
      ctx.drawImage(this.off, 0, 0, cols, rows,
        dev(ox), dev(oy), cols * cellPx, rows * cellPx);
    }

    this._drawOverlay(cur.overlay, t, ox, oy);

    requestAnimationFrame(this._raf);
  }

  _drawGlyph(glyph, unitX, unitY, unitScale = 3, alpha = 1) {
    const ctx = this.ctx;
    const s = Math.max(1, Math.round(unitScale * this.k));
    const x = Math.round(unitX * this.k), y = Math.round(unitY * this.k);
    ctx.globalAlpha = alpha;
    ctx.fillStyle = glyph.color;
    glyph.rows.forEach((row, r) => {
      for (let c = 0; c < row.length; c++) {
        if (row[c] === '#') ctx.fillRect(x + c * s, y + r * s, s, s);
      }
    });
    ctx.globalAlpha = 1;
  }

  _drawOverlay(kind, t, ox, oy) {
    if (!kind) return;
    const cx = ox + SPRITE_W * CELL / 2;
    switch (kind) {
      case 'dream': { // thought bubble replaying the working clip, nap-paced
        this._drawDream(t);
        break;
      }
      case 'dots': { // thought dots cycle (third dot + tilt must stay left of
        // unit ~141 — the Tauri viewport clips ~12px on the right)
        const n = 1 + (Math.floor(t * 2) % 3);
        for (let i = 0; i < n; i++) this._drawGlyph(GLYPHS.dot, ox + 76 + i * 12, oy - 12, 3);
        break;
      }
      case 'quest': { // steady question mark: blind is calm but clearly not-OK
        this._drawGlyph(GLYPHS.quest, cx - 8, oy - 28, 3);
        break;
      }
      case 'rain': { // error: a grey cloud hangs overhead, drops fall and fade
        const cloudX = cx - 14, cloudY = oy - 22;
        this._drawGlyph(GLYPHS.cloud, cloudX, cloudY, 4);
        for (let i = 0; i < 3; i++) {
          const phase = (t * 0.7 + i * 0.33) % 1;
          this._drawGlyph(GLYPHS.drop, cloudX + 4 + i * 9, cloudY + 12 + phase * 18, 2, 1 - phase * 0.7);
        }
        break;
      }
    }
  }

  // The dream: a thought bubble above the sleeper replaying the working clip
  // in miniature, paced to the whole nap — wind-up dance once at sleep onset,
  // the typing loop through the middle, and the grab-laptop/leap-home outro
  // timed to land exactly when the wake timer fires. An early wake just drops
  // the overlay mid-story.
  _drawDream(t) {
    const st = CLIPS.working.stages;
    const sum = (steps) => steps.reduce((a, s) => a + s.ms, 0);
    // no dream until the buddy has settled in (the sleeping intro): the
    // whole timeline shifts by the settle duration, and the natural-wake
    // sync still holds because the nap clock started at state entry too
    const settleMs = sum(CLIPS.sleeping.stages.intro);
    const el = t * 1000 - settleMs;
    if (el < 0) return;
    const introMs = sum(st.intro), loopMs = sum(st.loop), outroMs = sum(st.outro);
    const nap = (this._napMs || NAP_MAX_MS) - settleMs;
    let steps, ms;
    if (el < introMs) { steps = st.intro; ms = el; }
    else if (el < nap - outroMs) { steps = st.loop; ms = (el - introMs) % loopMs; }
    else { steps = st.outro; ms = Math.min(el - (nap - outroMs), outroMs - 1); }
    let step = steps[steps.length - 1];
    for (const s of steps) {
      if (ms < s.ms) { step = s; break; }
      ms -= s.ms;
    }

    // Wake-up choreography, driven by the outro clock (the outro carries the
    // dream overlay through the transition): the trail bubbles detach and
    // fade as the buddy rises, the cloud — dream still inside — floats up
    // off its head, and it bursts (POOF) during the morning stretch, fully
    // gone by the time the next state shows.
    let lift = 0, bubbleAlpha = 1;
    const outro = this._outro;
    if (outro) {
      // anchored to the wake's START (not its end): the cloud lifts as the
      // body rises and is fully dissolved before the arms-up stretch
      const oel = performance.now() - outro.start;
      const liftAt = 200, poofAt = liftAt + 333;
      if (oel >= poofAt) {
        if (oel < poofAt + 333) this._drawPoof((oel - poofAt) / 333);
        return;
      }
      // the bubble rises WITH the waking buddy, well clear of the risen
      // head (stretch head top is unit 68), and pops up high
      const lp = Math.min(1, Math.max(0, (oel - liftAt) / 333));
      lift = Math.round(lp * 28);
      bubbleAlpha = 1 - lp;
    }

    // cloud group (units): main blob + top puffs + trailing bubbles, with a
    // gentle 1-unit bob. Outline first, fills second, so overlaps merge into
    // one chunky outlined cloud; the outline keeps it readable on light desks.
    const ctx = this.ctx;
    const bob = Math.sin(t * 0.8) > 0 ? 1 : 0;
    const gdy = bob - lift;
    const u = (v) => Math.round(v * this.k);
    const rect = (x, y, w, h, dy) => ctx.fillRect(u(x), u(y + dy), u(x + w) - u(x), u(y + dy + h) - u(y + dy));
    const blob = (x, y, w, h, n) => { // chunky rounded rect: corner notch n
      rect(x + n, y, w - 2 * n, h, gdy);
      rect(x, y + n, w, h - 2 * n, gdy);
    };
    // hugs the sleeper: hovers just above the squashed body's top right
    // (head top is unit 96 under the droop). The Tauri viewport clips both
    // the canvas TOP strip and ~12px on the RIGHT (left-anchored wrap), so
    // the cloud stays left of unit ~145 and below y~45.
    const puffs = [
      [103, 52, 41, 28, 2],  // main body (interior for the scene)
      [107, 48, 10, 8, 1], [118, 46, 13, 10, 1], [132, 49, 9, 8, 1], // top scallops
    ];
    ctx.fillStyle = '#a89f98';
    puffs.forEach(([x, y, w, h, n]) => blob(x - 1, y - 1, w + 2, h + 2, n));
    ctx.fillStyle = '#fff3ea';
    puffs.forEach(([x, y, w, h, n]) => blob(x, y, w, h, n));
    // trail bubbles stay anchored to the head (bob only, no lift) and fade
    // out as the cloud detaches
    const bubbles = [[101, 82, 3], [96, 88, 2]];
    ctx.globalAlpha = bubbleAlpha;
    ctx.fillStyle = '#a89f98';
    bubbles.forEach(([x, y, s]) => rect(x - 1, y - 1, s + 2, s + 2, bob));
    ctx.fillStyle = '#fff3ea';
    bubbles.forEach(([x, y, s]) => rect(x, y, s, s, bob));
    ctx.globalAlpha = 1;

    // mini scene: the working frame at 1 unit per half-cell (quarter size),
    // bottom-anchored on the cloud floor; step dx scales down 4x (0 or -7)
    const frame = step.frame;
    const rows = frame.length, cols = frame[0].length;
    if (this.dreamOff.width !== cols || this.dreamOff.height !== rows) {
      this.dreamOff.width = cols;
      this.dreamOff.height = rows;
    }
    this.dreamCtx.clearRect(0, 0, cols, rows);
    const palette = PALETTES.normal;
    for (let r = 0; r < rows; r++) {
      const row = frame[r];
      for (let c = 0; c < cols; c++) {
        const ch = row[c];
        if (ch === '.') continue;
        this.dreamCtx.fillStyle = palette[ch] || '#ff00ff';
        this.dreamCtx.fillRect(c, r, 1, 1);
      }
    }
    const px = Math.max(1, Math.round(this.k)); // device px per half-cell
    const gx = 114 + step.dx / 4, gy = 76 - rows + gdy;
    ctx.drawImage(this.dreamOff, 0, 0, cols, rows, u(gx), u(gy), cols * px, rows * px);
  }

  // The bubble dissipates cloud-fashion: it splits into a few soft rounded
  // puffs (the same chunky blob shape as the cloud) that drift apart gently,
  // shrink and fade — mist, not shrapnel. p runs 0..1.
  _drawPoof(p) {
    const ctx = this.ctx;
    const u = (v) => Math.round(v * this.k);
    const rect = (x, y, w, h) => ctx.fillRect(u(x), u(y), u(x + w) - u(x), u(y + h) - u(y));
    const blob = (x, y, w, h) => {
      const n = Math.max(1, Math.round(Math.min(w, h) / 5));
      rect(x + n, y, w - 2 * n, h);
      rect(x, y + n, w, h - 2 * n);
    };
    // fragments of the lifted cloud (center 123,38): [cx, cy, w, h, driftX, driftY].
    // Drifts stay small and mostly upward; the right frag must not reach the
    // Tauri viewport's right clip (~unit 145+).
    const FRAGS = [
      [123, 38, 16, 11, 0, -2],
      [110, 34, 12, 9, -7, -3],
      [137, 36, 11, 8, 4, -2],
      [121, 26, 10, 8, 1, -7],
      [115, 46, 9, 7, -4, 3],
      [132, 45, 8, 6, 4, 3],
    ];
    const shrink = 1 - 0.7 * p;
    ctx.globalAlpha = 1 - p;
    for (const [cx, cy, w0, h0, dx, dy] of FRAGS) {
      const w = Math.max(2, Math.round(w0 * shrink)), h = Math.max(2, Math.round(h0 * shrink));
      const x = Math.round(cx + dx * p - w / 2), y = Math.round(cy + dy * p - h / 2);
      ctx.fillStyle = '#a89f98';
      blob(x - 1, y - 1, w + 2, h + 2);
      ctx.fillStyle = '#fff3ea';
      blob(x, y, w, h);
    }
    ctx.globalAlpha = 1;
  }
}
