// Art styles: everything the engine needs to know about one buddy design.
// `app` is the Claude app mascot (the original pet), `cli` the Claude Code
// terminal mascot. Both stand on the same ground line at the same width, so
// panels, the "+" button and the hit region line up whichever is worn.
//
//   clips     the state -> clip table (sprites.js / sprites-cli.js)
//   spriteW/H base grid of the flat clips, in cells
//   cell      canvas units per base cell (app 8, cli 1.5 -> both 96 wide)
//   ground    canvas unit the sprite's bottom row sits on
//   snap      how cells hit device pixels: 'int' = one integer number of
//             device px per cell (the app's crisp 8-unit cells); 'edge' =
//             cell edges rounded to device px (the cli's 1.5-unit cells)
//   envelope  worst-case motion offsets around the rest rect, in units
//   anchors   overlay positions: dots/quest/rain relative to the frame origin
//             (ox, oy) or the sprite centre (cx); dream = the mini scene's
//             origin and units per frame cell inside the thought cloud
'use strict';

import { CLIPS, SPRITE_W, SPRITE_H } from './sprites.js';
import { CLIPS_CLI, CLI_W, CLI_H } from './sprites-cli.js';

const UNIT_W = 160;

// Worst-case motion offsets measured over every frame of every clip: the
// frame art's extent at its step dx/dy against the rest rect, plus the
// engine motions (hop up 8, droop down 4, tilt/wobble/work +-2).
function measureEnvelope(clips, spriteW, spriteH, cell, ground) {
  const restW = spriteW * cell, restH = spriteH * cell;
  const restX = (UNIT_W - restW) / 2, restY = ground - restH;
  let left = 0, right = 0, up = 0, down = 0;
  for (const clip of Object.values(clips)) {
    const res = clip.res || 1, uc = cell / res;
    const steps = clip.frames ? clip.frames.map((f) => ({ frame: f, dx: 0, dy: 0 })) : [...clip.stages.intro, ...clip.stages.loop, ...clip.stages.outro];
    for (const s of steps) {
      const f = s.frame;
      let minC = Infinity, maxC = -1, minR = Infinity, maxR = -1;
      f.forEach((row, r) => {
        for (let c = 0; c < row.length; c++) if (row[c] !== '.') { minC = Math.min(minC, c); maxC = Math.max(maxC, c); minR = Math.min(minR, r); maxR = Math.max(maxR, r); }
      });
      if (maxC < 0) continue;
      const ox = restX + s.dx, oy = ground - f.length * uc + s.dy;
      left = Math.max(left, restX - (ox + minC * uc));
      right = Math.max(right, ox + (maxC + 1) * uc - (restX + restW));
      up = Math.max(up, restY - (oy + minR * uc));
      down = Math.max(down, oy + (maxR + 1) * uc - (restY + restH));
    }
  }
  return { left: Math.ceil(left) + 2, right: Math.ceil(right) + 2, up: Math.ceil(up) + 8, down: Math.ceil(down) + 4 };
}

const STYLES = {
  app: {
    name: 'app', clips: CLIPS, spriteW: SPRITE_W, spriteH: SPRITE_H, cell: 8, ground: 148, snap: 'int',
    envelope: { left: 28, right: 24, up: 48, down: 4 },
    anchors: {
      dots: { x: 76, y: -12, step: 12 },
      quest: { x: -8, y: -28 },
      rain: { x: -14, y: -22 },
      dream: { x: 114, y: 76, cell: 1 },
    },
  },
  cli: {
    name: 'cli', clips: CLIPS_CLI, spriteW: CLI_W, spriteH: CLI_H, cell: 1.5, ground: 146, snap: 'edge',
    envelope: measureEnvelope(CLIPS_CLI, CLI_W, CLI_H, 1.5, 146),
    // the CLI head top sits 4 units lower in the frame than the app's (its
    // 8-unit headroom is 12 canvas units), so head-relative overlays drop 4
    anchors: {
      dots: { x: 76, y: -8, step: 12 },
      quest: { x: -8, y: -24 },
      rain: { x: -14, y: -18 },
      // 84-cell away frames at 0.4 units per cell = 34 units wide, inside
      // the cloud's 41-unit interior; step dx scales by 0.4/1.5
      dream: { x: 114, y: 76, cell: 0.4 },
    },
  },
};

export { STYLES };
