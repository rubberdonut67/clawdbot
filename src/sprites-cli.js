// The CLI-style buddy: the Claude Code terminal mascot ("Clawd"), measured
// from the user's reference still and cross-checked against the announcement
// GIF (every edge of both lands on the same grid). Unit grid 64 wide x 40
// tall (1 unit = 1.5 canvas units, so the body is 96 units wide like the app
// buddy and stands on the same ground line):
//
//   head      cols 8-55, rows 0-31 (48 x 32)
//   arm band  cols 0-63, rows 16-23 (8-unit nubs each side)
//   eyes      3 x 8, cols 16-18 and 45-47, rows 8-15 (8 in from the head edge)
//   legs      4 x (5 x 8): cols 11-15, 19-23, 40-44, 48-52, rows 32-39
//
// The base frame adds 8 rows of motion headroom above (stretch, raised
// arms) and 4 below (ground headroom), like the app grid's rows 0 and 9.
// Frames are BUILT from rectangles on this grid rather than typed as ASCII —
// 64x52 art is too big to hand-edit safely — and the away-from-home working
// poses are re-projected from the measured app frames (sprites.js) so the
// choreography stays the measured one; only the body proportions change.
// Same clip vocabulary as the app style; soccer (demo-only) is left out.
'use strict';

import { validateClips } from './sprite-kit.js';
import { WORK_AWAY, FLAG_ZONES } from './sprites.js';

const CLI_W = 64, CLI_TOP = 8, CLI_BODY_H = 40, CLI_BOT = 4;
const CLI_H = CLI_TOP + CLI_BODY_H + CLI_BOT; // 52

const HEAD_X = 8, HEAD_W = 48, HEAD_H = 32, BAND_Y = 16, ROW = 8, NUB = 8;
const EYE_XS = [16, 45], EYE_W = 3, EYE_H = 8, EYE_Y = 8;
const LEG_XS = [11, 19, 40, 48], LEG_W = 5, LEG_H = 8;

// --- grid primitives ---
const grid = (w, h) => Array.from({ length: h }, () => Array(w).fill('.'));
const put = (g, x, y, w, h, ch = '#') => {
  for (let r = 0; r < h; r++) {
    const row = g[y + r];
    if (!row) continue;
    for (let c = 0; c < w; c++) if (x + c >= 0 && x + c < row.length) row[x + c] = ch;
  }
};
const rows = (g) => g.map((r) => r.join(''));

// --- body: head + band (nubs) + legs, in frame coordinates. `sink` lowers
// the whole body (squash/crouch), `legH` the leg length (0 = tucked),
// `bandDy` sags the arm nubs (droop), `nubs` which side nubs to draw,
// `bars` which raised-arm bars stand on the head corners (bounceUp/wave). ---
function body({ sink = 0, legH = LEG_H, bandDy = 0, nubs = 'both', bars = [], top = CLI_TOP, w = CLI_W, h = CLI_H, legShift = 0 } = {}) {
  const g = grid(w, h);
  const t = top + sink;
  put(g, HEAD_X, t, HEAD_W, HEAD_H);
  const by = t + BAND_Y + bandDy;
  if (nubs === 'both' || nubs === 'left') put(g, 0, by, NUB, ROW);
  if (nubs === 'both' || nubs === 'right') put(g, HEAD_X + HEAD_W, by, NUB, ROW);
  for (const b of bars) put(g, b === 'L' ? HEAD_X : HEAD_X + HEAD_W - NUB, t - ROW, NUB, ROW);
  if (legH > 0) {
    const ly = t + HEAD_H;
    if (!legShift) for (const x of LEG_XS) put(g, x, ly, LEG_W, legH);
    else { // leaning: the lower half of every leg steps sideways so the feet stay planted
      const half = legH >> 1;
      for (const x of LEG_XS) { put(g, x, ly, LEG_W, half); put(g, x + legShift, ly + half, LEG_W, legH - half); }
    }
  }
  return g;
}

// --- faces, stamped over the body. Eye top row follows the body's sink. ---
const eyeTop = (sink = 0, dy = 0, top = CLI_TOP) => top + sink + EYE_Y + dy;
const eyesOpen = (g, { sink = 0, dx = 0, dy = 0, top = CLI_TOP } = {}) => {
  for (const x of EYE_XS) put(g, x + dx, eyeTop(sink, dy, top), EYE_W, EYE_H, 'E');
  return g;
};
// happy closed ∩: 2-unit strokes, 9 wide x 5 tall, centred on the eye box
const eyesArch = (g, { sink = 0, dy = 0, top = CLI_TOP } = {}) => {
  const y = eyeTop(sink, dy, top);
  for (const x of EYE_XS) {
    put(g, x - 1, y + 1, 5, 2, 'E');
    put(g, x - 3, y + 3, 2, 3, 'E');
    put(g, x + 4, y + 3, 2, 3, 'E');
  }
  return g;
};
// sad closed ∪: the exact vertical mirror of the arch
const eyesSad = (g, { sink = 0, dy = 0, top = CLI_TOP } = {}) => {
  const y = eyeTop(sink, dy, top);
  for (const x of EYE_XS) {
    put(g, x - 3, y + 2, 2, 3, 'E');
    put(g, x + 4, y + 2, 2, 3, 'E');
    put(g, x - 1, y + 5, 5, 2, 'E');
  }
  return g;
};
// held-closed slit (sleep, half-open wake moments, the crouch squint)
const eyesSlit = (g, { sink = 0, dy = 0, top = CLI_TOP, which = 'both' } = {}) => {
  const y = eyeTop(sink, dy, top);
  EYE_XS.forEach((x, i) => {
    if (which === 'both' || (which === 'left' && i === 0) || (which === 'right' && i === 1)) put(g, x - 2, y + 4, 7, 2, 'E');
  });
  return g;
};
const tearAt = (g, y) => { put(g, EYE_XS[0], y, 2, 2, 'w'); return g; };

// --- composed flat frames (64 x 52) ---
const IDLE_OPEN   = rows(eyesOpen(body()));
const IDLE_SIDE   = rows(eyesOpen(body(), { dx: 3 }));
const IDLE_SIDE_L = rows(eyesOpen(body(), { dx: -3 }));
const IDLE_BLINK  = rows(body());
const THINK_UP    = rows(eyesOpen(body(), { dy: -4 }));
// error: drooped arms, ∪ eyes, a white tear under the left eye sliding down
const errorBody = () => eyesSad(body({ bandDy: 4 }));
const ERROR_SAD_A = rows(tearAt(errorBody(), eyeTop() + EYE_H + 1));
const ERROR_SAD_B = rows(tearAt(errorBody(), eyeTop() + EYE_H + 4));
// sleep: sunk onto the ground, legs tucked, slit eyes
const SLEEP_CLOSED    = rows(eyesSlit(body({ sink: 8, legH: 0 }), { sink: 8 }));
const SLEEP_DROWSY    = rows(eyesSlit(body({ bandDy: 4 })));
const WAKE_SLIT       = rows(eyesSlit(body()));
const WAKE_STRETCH    = rows(eyesOpen(body({ sink: -4, legH: 12 }), { sink: -4 }));
const WAKE_BIGSTRETCH = rows(eyesSlit(body({ nubs: 'none', bars: ['L', 'R'] })));
// needs_input hop + slow both-arms raise, all with the happy ∩
const HOP_STRETCH = rows(eyesArch(body({ sink: -4, legH: 12 }), { sink: -4 }));
const HOP_NORMAL  = rows(eyesArch(body()));
const HOP_SQUASH  = rows(eyesArch(body({ sink: 8, legH: 0 }), { sink: 8 }));
const HOP_ARMSUP  = rows(eyesArch(body({ nubs: 'none', bars: ['L', 'R'] })));
const WAVE_UP     = rows(eyesArch(body({ nubs: 'left', bars: ['R'] })));

// --- working, home frames: the wind-up dance and the landing, drawn on the
// CLI body from the app's measured poses (sprites.js WORK_*). Arm blocks
// are band-height (8) and nub-wide (8); one app half-row = 2 units on this
// body, one app half-col outside the head = 2 units. arm() takes a body
// row for y and adds the headroom itself. ---
const arm = (g, x, y, w = NUB, h = ROW) => put(g, x, CLI_TOP + y, w, h);
const RIGHT = HEAD_X + HEAD_W; // 56

const W_REST = IDLE_OPEN;
// crouch-bounce: sunk 2, legs short, right arm up and out, left arm low,
// eyes lower in the head, right eye a squint
const crouch = (leftArmY, squint) => {
  const g = body({ sink: 2, legH: 6, nubs: 'none' });
  arm(g, RIGHT, 2 + BAND_Y - 4);
  arm(g, 0, 2 + BAND_Y + leftArmY);
  eyesOpen(g, { sink: 2, dy: 4 });
  if (squint) { put(g, EYE_XS[1], eyeTop(2, 4), EYE_W, EYE_H, '#'); eyesSlit(g, { sink: 2, dy: 4, which: 'right' }); }
  return rows(g);
};
const W_CROUCH_A  = crouch(4, true);
const W_CROUCH_B  = crouch(6, true);
const W_LANDHOME  = crouch(6, false);
// left arm swings up level with the eyes (a bar from eye level to the band,
// tapering back into the body), right arm out
const W_UP = (() => {
  const g = body({ nubs: 'none' });
  arm(g, 0, EYE_Y, NUB, 12);
  arm(g, 2, EYE_Y + 12, 6, 2);
  arm(g, 4, EYE_Y + 14, 4, 2);
  arm(g, RIGHT, BAND_Y, NUB, 10);
  return rows(eyesOpen(g));
})();
// stretched: left arm thrown high (a bar from above the head down to eye
// level), right arm out a touch below the band
const W_STRETCH = (() => {
  const g = body({ nubs: 'none' });
  arm(g, 0, -2, NUB, 14);
  arm(g, RIGHT, BAND_Y + 2);
  return rows(eyesOpen(g));
})();
// settling: arms come back down in three beats, eyes a little lower
const W_SET1 = (() => {
  const g = body({ nubs: 'none' });
  arm(g, RIGHT, BAND_Y, 4, 2); arm(g, RIGHT, BAND_Y + 2, NUB, 6);
  arm(g, 2, BAND_Y + 2, 6, 8); arm(g, 0, BAND_Y + 10, NUB, 6);
  return rows(eyesOpen(g, { dy: 4 }));
})();
const W_SET2 = (() => {
  const g = body({ nubs: 'none' });
  arm(g, RIGHT, BAND_Y + 2);
  arm(g, 0, BAND_Y + 10, NUB, 6);
  return rows(eyesOpen(g, { dy: 4 }));
})();
const W_SET3 = (() => {
  const g = body({ nubs: 'none' });
  arm(g, RIGHT, BAND_Y - 2);
  arm(g, 0, BAND_Y + 2, NUB, 6);
  return rows(eyesOpen(g, { dy: 4 }));
})();

// --- working, away frames: re-projected from the app's measured turned
// poses. Horizontally every app half-col becomes 3 units (the app head's
// 16 half-cols -> this head's 48). Vertically the first four rows of the
// head (head top + eye rows) become 4 units each and every other row 2
// units, which turns the app's 12-half-row head into this body's 32 and
// the 4-half-row legs into 8 — the CLI's squatter proportions. Eyes are
// re-drawn as this body's 3-wide eyes (a 1-row squint -> the slit). ---
const AWAY_W = 28 * 3; // the measured frames' art ends at half-col 27
function reproject(src) {
  const isArt = (ch) => ch !== '.' && ch !== 'L';
  let ht = 0;
  outer: for (let r = 0; r < src.length; r++) {
    let run = 0;
    for (const ch of src[r]) { run = isArt(ch) ? run + 1 : 0; if (run >= 12) { ht = r; break outer; } }
  }
  const rowH = (r) => (r >= ht && r < ht + 4 ? 4 : 2);
  const ys = [];
  let y = 0;
  for (let r = 0; r < src.length; r++) { ys.push(y); y += rowH(r); }
  const g = grid(AWAY_W, y);
  for (let r = 0; r < src.length; r++) {
    for (let c = 0; c < src[r].length; c++) {
      const ch = src[r][c];
      if (ch !== '.') put(g, c * 3, ys[r], 3, rowH(r), ch === 'E' ? '#' : ch);
    }
  }
  // eye blobs (4-connected E regions) -> CLI eyes
  const seen = src.map((r) => Array(r.length).fill(false));
  for (let r = 0; r < src.length; r++) {
    for (let c = 0; c < src[r].length; c++) {
      if (src[r][c] !== 'E' || seen[r][c]) continue;
      let minR = r, maxR = r, minC = c, maxC = c;
      const stack = [[r, c]];
      seen[r][c] = true;
      while (stack.length) {
        const [pr, pc] = stack.pop();
        minR = Math.min(minR, pr); maxR = Math.max(maxR, pr); minC = Math.min(minC, pc); maxC = Math.max(maxC, pc);
        for (const [dr, dc] of [[1, 0], [-1, 0], [0, 1], [0, -1]]) {
          const nr = pr + dr, nc = pc + dc;
          if (src[nr] && src[nr][nc] === 'E' && !seen[nr][nc]) { seen[nr][nc] = true; stack.push([nr, nc]); }
        }
      }
      const cx = ((minC + maxC + 1) * 3) / 2;
      if (maxR === minR) put(g, Math.round(cx) - 3, ys[minR], 7, 2, 'E');
      else put(g, Math.round(cx - 1.5), ys[minR], EYE_W, EYE_H, 'E');
    }
  }
  return rows(g);
}
const W_LEAP     = reproject(WORK_AWAY.LEAP);
const W_LAND     = reproject(WORK_AWAY.LAND);
const W_HAM_A    = reproject(WORK_AWAY.HAM_A);
const W_HAM_B    = reproject(WORK_AWAY.HAM_B);
const W_HAM_C    = reproject(WORK_AWAY.HAM_C);
const W_GRAB_B   = reproject(WORK_AWAY.GRAB_B);
const W_GRAB_C   = reproject(WORK_AWAY.GRAB_C);
const W_RISE     = reproject(WORK_AWAY.RISE);
const W_LEAPBACK = reproject(WORK_AWAY.LEAPBACK);

// --- done: the checkered-flag wave. The pole and cloth zones are the app's
// (body-independent), scaled x3 so one app half-cell is 3 units and shifted
// 2 units left so the pole rises from inside this head's edge; the raised
// right arm is redrawn as this body's 8-wide bar standing on the head's
// right corner (its height follows the zone: the apex punches higher).
// Bodies: no top headroom (the zone sits directly on the head), ∩ eyes
// baked in, leans as in the app (lower legs step against the sway; the
// left arm rides lower on the left lean). The right arm IS the raised bar,
// so the right side nub is absent (user caught a doubled arm). ---
const FLAG_W = 74, ZONE_H = 14 * 3;
function zone3(zone) {
  const g = grid(FLAG_W, ZONE_H);
  let armTop = -1;
  zone.forEach((row, r) => {
    for (let c = 0; c < row.length; c++) {
      const ch = row[c];
      if (ch === 'E' || ch === 'w') put(g, 3 * c - 2, 3 * r, 3, 3, ch);
      else if (ch === '#' && armTop < 0) armTop = r;
    }
  });
  if (armTop >= 0) put(g, HEAD_X + HEAD_W - NUB, 3 * armTop, NUB, ZONE_H - 3 * armTop);
  return rows(g);
}
const flagBody = (opts) => rows(eyesArch(body({ top: 0, w: FLAG_W, h: CLI_BODY_H + CLI_BOT, ...opts }), { top: 0 }));
const FLAG_BODY_C = flagBody({ nubs: 'left' });
const FLAG_BODY_R = flagBody({ nubs: 'left', legShift: -3 });
const FLAG_BODY_L = (() => {
  const g = body({ top: 0, w: FLAG_W, h: CLI_BODY_H + CLI_BOT, legShift: 3, nubs: 'none' });
  put(g, 0, BAND_Y + 3, NUB, ROW); // left arm rides lower
  return rows(eyesArch(g, { top: 0 }));
})();
const flagFrame = (zone, bodyRows) => [...zone3(zone), ...bodyRows];
const FLAG_A = flagFrame(FLAG_ZONES.A, FLAG_BODY_C);
const FLAG_B = flagFrame(FLAG_ZONES.B, FLAG_BODY_R);
const FLAG_C = flagFrame(FLAG_ZONES.C, FLAG_BODY_R);
const FLAG_D = flagFrame(FLAG_ZONES.D, FLAG_BODY_R);
const FLAG_E = flagFrame(FLAG_ZONES.E, FLAG_BODY_C);
const FLAG_F = flagFrame(FLAG_ZONES.F, FLAG_BODY_C);
const FLAG_G = flagFrame(FLAG_ZONES.G, FLAG_BODY_L);
const FLAG_H = flagFrame(FLAG_ZONES.H, FLAG_BODY_L);
const FLAG_I = flagFrame(FLAG_ZONES.I, FLAG_BODY_L);

// --- clips: the app's tables (fps, stages, 60Hz-aligned ms, dx/dy in canvas
// units) with CLI frames. See sprites.js for the choreography notes. ---
const CLIPS_CLI = {
  sleeping: {
    palette: 'cli', motion: 'droop', overlay: 'dream',
    stages: {
      intro: [
        { frame: IDLE_OPEN,    dx: 0,  dy: -4, ms: 133 },
        { frame: WAKE_SLIT,    dx: 0,  dy: -4, ms: 200 },
        { frame: SLEEP_DROWSY, dx: 0,  dy: -3, ms: 200 },
        { frame: SLEEP_CLOSED, dx: 0,  dy: -2, ms: 133 },
        { frame: SLEEP_CLOSED, dx: -2, dy: 0,  ms: 133 },
        { frame: SLEEP_CLOSED, dx: 2,  dy: 0,  ms: 133 },
      ],
      loop: [{ frame: SLEEP_CLOSED, dx: 0, dy: 0, ms: 1000 }],
      outro: [
        { frame: SLEEP_CLOSED,    dx: 0, dy: 4, ms: 200 },
        { frame: SLEEP_CLOSED,    dx: 0, dy: 2, ms: 67 },
        { frame: WAKE_SLIT,       dx: 0, dy: 0, ms: 133 },
        { frame: IDLE_OPEN,       dx: 0, dy: 0, ms: 67 },
        { frame: WAKE_STRETCH,    dx: 0, dy: 0, ms: 133 },
        { frame: WAKE_BIGSTRETCH, dx: 0, dy: 0, ms: 600 },
        { frame: WAKE_STRETCH,    dx: 0, dy: 0, ms: 133 },
        { frame: IDLE_OPEN,       dx: 0, dy: 0, ms: 133 },
      ],
    },
  },
  idle: {
    fps: 2, palette: 'cli', motion: 'breathe', overlay: null,
    frames: [
      ...Array(14).fill(IDLE_OPEN),
      IDLE_BLINK,
      ...Array(8).fill(IDLE_OPEN),
      ...Array(2).fill(IDLE_SIDE),
      IDLE_BLINK,
      ...Array(12).fill(IDLE_OPEN),
      IDLE_BLINK,
      ...Array(7).fill(IDLE_OPEN),
      ...Array(2).fill(IDLE_SIDE_L),
      IDLE_BLINK,
      ...Array(2).fill(IDLE_OPEN),
    ],
  },
  working: {
    palette: 'cli', overlay: null, motion: 'none',
    stages: {
      intro: [
        { frame: W_REST,     dx: 0,  dy: 0, ms: 200 },
        { frame: W_CROUCH_A, dx: 0,  dy: 0, ms: 67 },
        { frame: W_CROUCH_B, dx: 0,  dy: 0, ms: 67 },
        { frame: W_CROUCH_A, dx: 0,  dy: 0, ms: 133 },
        { frame: W_CROUCH_B, dx: 0,  dy: 0, ms: 133 },
        { frame: W_UP,       dx: 0,  dy: 0, ms: 133 },
        { frame: W_STRETCH,  dx: 0,  dy: 0, ms: 133 },
        { frame: W_SET1,     dx: 0,  dy: 0, ms: 67 },
        { frame: W_SET2,     dx: 0,  dy: 0, ms: 133 },
        { frame: W_SET3,     dx: 0,  dy: 0, ms: 67 },
        { frame: W_LEAP,     dx: -28, dy: 0, ms: 67 },
        { frame: W_LAND,     dx: -28, dy: 0, ms: 67 },
      ],
      loop: [
        { frame: W_HAM_B, dx: -28, dy: 0, ms: 67 },
        { frame: W_HAM_C, dx: -28, dy: 0, ms: 133 },
        { frame: W_HAM_A, dx: -28, dy: 0, ms: 67 },
      ],
      outro: [
        { frame: W_GRAB_B,   dx: -28, dy: 0, ms: 133 },
        { frame: W_GRAB_C,   dx: -28, dy: 0, ms: 67 },
        { frame: W_RISE,     dx: -28, dy: 0, ms: 67 },
        { frame: W_LEAPBACK, dx: -28, dy: 0, ms: 67 },
        { frame: W_LANDHOME, dx: 0,   dy: 0, ms: 200 },
      ],
    },
  },
  thinking: {
    fps: 2, palette: 'cli', motion: 'tilt', overlay: 'dots',
    frames: [THINK_UP, THINK_UP],
  },
  needs_input: {
    fps: 6, palette: 'cli', motion: 'hopOnce', overlay: null,
    frames: [
      HOP_STRETCH, HOP_NORMAL, HOP_SQUASH,
      ...Array(4).fill(HOP_NORMAL),
      ...Array(4).fill(HOP_ARMSUP),
      ...Array(9).fill(HOP_NORMAL),
      ...Array(4).fill(HOP_ARMSUP),
      ...Array(14).fill(HOP_NORMAL),
      ...Array(5).fill(HOP_ARMSUP),
      ...Array(8).fill(HOP_NORMAL),
    ],
  },
  done: {
    palette: 'cli', overlay: null, motion: 'none',
    stages: {
      intro: [],
      loop: [
        { frame: FLAG_A, dx: 0,  dy: 0, ms: 67 },
        { frame: FLAG_B, dx: 4,  dy: 0, ms: 67 },
        { frame: FLAG_C, dx: 4,  dy: 0, ms: 67 },
        { frame: FLAG_D, dx: 4,  dy: 0, ms: 67 },
        { frame: FLAG_E, dx: 0,  dy: 0, ms: 67 },
        { frame: FLAG_F, dx: 0,  dy: 0, ms: 67 },
        { frame: FLAG_G, dx: -4, dy: 0, ms: 67 },
        { frame: FLAG_H, dx: -4, dy: 0, ms: 67 },
        { frame: FLAG_I, dx: -4, dy: 0, ms: 67 },
      ],
      outro: [],
    },
  },
  error: {
    fps: 2, palette: 'cli', motion: 'breathe', overlay: 'rain',
    frames: [ERROR_SAD_A, ERROR_SAD_B],
  },
  blind: {
    fps: 1, palette: 'grey', motion: 'droop', overlay: 'quest',
    frames: [IDLE_OPEN, IDLE_BLINK],
  },
  wave: {
    palette: 'cli', motion: 'none', overlay: null,
    stages: {
      intro: [],
      loop: [
        { frame: WAVE_UP,    dx: 0, dy: 0, ms: 250 },
        { frame: HOP_NORMAL, dx: 0, dy: 0, ms: 250 },
      ],
      outro: [{ frame: HOP_NORMAL, dx: 0, dy: 0, ms: 267 }],
    },
  },
};

validateClips(CLIPS_CLI, CLI_W, CLI_H);

export { CLIPS_CLI, CLI_W, CLI_H };
