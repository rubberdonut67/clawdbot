// Shared sprite toolkit: palettes, overlay glyphs, and the frame-composition
// helpers every art style builds its clips with. Style-neutral — the app
// buddy (sprites.js) and the CLI buddy (sprites-cli.js) both import from here
// and nothing in this file knows either body's grid.
//
// Char map: . transparent  # body  % darker body tone  E eye  w eye shine /
// tear  L laptop grey
'use strict';

const PALETTES = {
  // '%' is the darker companion tone (closed eyelids; the working clip's
  // back-shade). 'L' is the little laptop's grey (working clip only).
  normal: { '#': '#da7758', '%': '#b25a42', 'E': '#000000', 'w': '#fff3ea', 'L': '#7d7d7d' },
  // blind must never read as idle: all body color drains out
  grey:   { '#': '#9a9a9a', '%': '#7d7d7d', 'E': '#2e2e2e', 'w': '#dddddd' },
  // the CLI buddy: body sampled from the user's reference (#d47f5a); the
  // shade keeps the measured bright:dark ratio of the recording's two tones
  cli:    { '#': '#d47f5a', '%': '#a96d4f', 'E': '#000000', 'w': '#fff3ea', 'L': '#7d7d7d' },
};

// --- overlay glyphs, drawn by the engine near the buddy (own colors) ---

const GLYPHS = {
  quest:    { color: '#e0e0e0', rows: ['.###.', '#...#', '...#.', '..#..', '.....', '..#..'] },
  dot:      { color: '#c9b8ae', rows: ['##', '##'] },
  cloud:    { color: '#9a9a9a', rows: ['.#####.', '#######', '#######'] },
  drop:     { color: '#bfbfbf', rows: ['#', '#'] },
};

function stamp(body, face) {
  const out = body.rows.map((r) => r.split(''));
  if (face) {
    face.rows.forEach((fr, i) => {
      const r = face.row + body.dy + i;
      for (let j = 0; j < fr.length; j++) {
        if (fr[j] !== '.') out[r][face.col + j] = fr[j];
      }
    });
  }
  return out.map((r) => r.join(''));
}

// --- half-cell eye handling for the res-2 flat clips. dbl() doubles a
// composed grid to half-cell resolution. A blink is a FULL eye vanish —
// bare body, no slit or line (user rule); slitEyes() draws the thin
// closed-eye slits for held-closed eyes only (the sleeping pose and the
// wake-up transition's half-open moment). ---

const dbl = (rows) => rows.flatMap((r) => {
  const d = r.split('').map((ch) => ch + ch).join('');
  return [d, d];
});
const slitEyes = (grid, row, cols) => {
  const out = grid.map((r) => r.split(''));
  cols.forEach((c) => { out[row][c] = 'E'; out[row][c + 1] = 'E'; });
  return out.map((r) => r.join(''));
};
// archEyes() draws the happy closed ∩-arch exactly as the needs_input flag
// bodies bake it: a 2-wide top bar across the eye cell's upper half-row,
// then legs one half-col outside the bar on the lower half-row.
const archEyes = (grid, row, cols) => {
  const out = grid.map((r) => r.split(''));
  cols.forEach((c) => {
    out[row][c] = 'E'; out[row][c + 1] = 'E';
    out[row + 1][c - 1] = 'E'; out[row + 1][c + 2] = 'E';
  });
  return out.map((r) => r.join(''));
};
// sadEyes() is the inverted arch (∪), the exact mirror of the happy ∩:
// bottom bar 2-wide on the eye cell's lower half-row, legs one half-col
// outside on the upper half-row. Closed and very sad.
const sadEyes = (grid, row, cols) => {
  const out = grid.map((r) => r.split(''));
  cols.forEach((c) => {
    out[row + 1][c] = 'E'; out[row + 1][c + 1] = 'E';
    out[row][c - 1] = 'E'; out[row][c + 2] = 'E';
  });
  return out.map((r) => r.join(''));
};
// a single white tear cell
const tear = (grid, row, col) => {
  const out = grid.map((r) => r.split(''));
  out[row][col] = 'w';
  return out.map((r) => r.join(''));
};
const mirrorPose = (rows) => rows.map((r) => r.split('').reverse().join(''));

// Load-time validation: a malformed frame must fail loudly, not render garbage.
// Staged clips carry self-sized frames (any dims, uniform rows); flat clips
// must match the style's base sprite grid (times the clip's res).
function allFrames(clip) {
  if (clip.frames) return clip.frames;
  const st = clip.stages;
  return [...st.intro, ...st.loop, ...st.outro].map((s) => s.frame);
}
function validateClips(clips, spriteW, spriteH) {
  for (const [name, clip] of Object.entries(clips)) {
    const res = clip.res || 1;
    for (const frame of allFrames(clip)) {
      if (!clip.stages && frame.length !== spriteH * res) throw new Error(`clip ${name}: frame has ${frame.length} rows`);
      for (const row of frame) {
        if (!clip.stages && row.length !== spriteW * res) throw new Error(`clip ${name}: row "${row}" is ${row.length} wide`);
        if (row.length !== frame[0].length) throw new Error(`clip ${name}: ragged frame rows`);
        for (const ch of row) {
          if (ch !== '.' && !(ch in PALETTES[clip.palette])) {
            throw new Error(`clip ${name}: char "${ch}" missing from palette ${clip.palette}`);
          }
        }
      }
    }
  }
}

export { PALETTES, GLYPHS, stamp, dbl, slitEyes, archEyes, sadEyes, tear, mirrorPose, validateClips };
