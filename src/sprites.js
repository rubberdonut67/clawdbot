// The buddy, measured cell-by-cell from the user's reference image
// (src/reference.png, 12x8 character grid @ ~18.3px/cell, body #DA7758):
// flat 8-wide body (cols 2-9, no corner notches), single-cell black eyes at
// cols 3 & 8 on the second body row, full-width arm band directly below the
// eyes (2x2 arm nubs), two PAIRS of 1x2 legs at cols 2/4/7/9 — the outer
// legs sit flush with the body sides.
//
// Frames are composed matrices; final art can swap in behind the same CLIPS
// interface — nothing else may depend on how these frames are authored.
//
// Char map: . transparent  # body  E eye  w eye shine
//
// This is the APP style (the Claude app mascot). The palettes, glyphs and
// composition helpers live in sprite-kit.js, shared with the CLI style in
// sprites-cli.js; the clips here are unchanged by that split.
'use strict';

import { PALETTES, GLYPHS, stamp, dbl, slitEyes, archEyes, sadEyes, tear, mirrorPose, validateClips } from './sprite-kit.js';

// --- bodies: 12 wide x 10 tall (rows 0 & 9 are motion headroom).
// Normal pose: body rows 1-6, arm band rows 3-4, legs rows 7-8.
// dy shifts face stamps with the body. ---

const BODIES = {
  normal: { dy: 0, rows: [
    '............',
    '..########..',
    '..########..',
    '############',
    '############',
    '..########..',
    '..########..',
    '..#.#..#.#..',
    '..#.#..#.#..',
    '............',
  ]},
  // frazzled/dejected: arms sag one row
  droop: { dy: 0, rows: [
    '............',
    '..########..',
    '..########..',
    '..########..',
    '############',
    '############',
    '..########..',
    '..#.#..#.#..',
    '..#.#..#.#..',
    '............',
  ]},
  // sitting on the ground, legs tucked
  squash: { dy: 2, rows: [
    '............',
    '............',
    '............',
    '..########..',
    '..########..',
    '############',
    '############',
    '..########..',
    '..########..',
    '............',
  ]},
  // airborne: body up, legs extended
  stretch: { dy: -1, rows: [
    '..########..',
    '..########..',
    '############',
    '############',
    '..########..',
    '..########..',
    '..#.#..#.#..',
    '..#.#..#.#..',
    '..#.#..#.#..',
    '............',
  ]},
  // arms thrown UP: floating bars above the body corners (the approved
  // WORK_LEAP arms-up look), no side nubs on the band rows — used for the
  // needs_input slow arm raise
  bounceUp: { dy: 0, rows: [
    '..##....##..',
    '..########..',
    '..########..',
    '..########..',
    '..########..',
    '..########..',
    '..########..',
    '..#.#..#.#..',
    '..#.#..#.#..',
    '............',
  ]},
  // one-armed bounceUp for the wave: the RIGHT arm is the raised bar
  // (identical to bounceUp's right bar) while the LEFT stays a side nub —
  // strictly the approved vocabulary, one-sided
  waveUp: { dy: 0, rows: [
    '........##..',
    '..########..',
    '..########..',
    '##########..',
    '##########..',
    '..########..',
    '..########..',
    '..#.#..#.#..',
    '..#.#..#.#..',
    '............',
  ]},
};

// --- faces: stamped at (row + body.dy, col). '.' keeps the body pixel.
// Normal eyes: single cells at cols 3 & 8, row 2 (matches the reference). ---

const FACES = {
  normal: { row: 2, col: 3, rows: ['E....E'] },
  side:   { row: 2, col: 4, rows: ['E....E'] },   // glance right
  sideL:  { row: 2, col: 2, rows: ['E....E'] },   // glance left
  up:     { row: 1, col: 3, rows: ['E....E'] },   // pondering
};

// --- composed frames (helpers from sprite-kit.js). A blink is a FULL eye
// vanish — bare body, no slit or line (user rule); slits are for held-closed
// eyes only (sleeping, the wake-up's half-open moment). ---

// base eyes: cells (2,3)&(2,8) -> doubled rows 4-5, cols 6-7 & 16-17
const IDLE_OPEN    = dbl(stamp(BODIES.normal, FACES.normal));
const IDLE_SIDE    = dbl(stamp(BODIES.normal, FACES.side));
const IDLE_SIDE_L  = dbl(stamp(BODIES.normal, FACES.sideL));
const IDLE_BLINK   = dbl(stamp(BODIES.normal, null));
// error: drooped body with ∪ sad eyes (droop dy 0 -> eye row 2 -> rows 4-5)
// plus a single white tear under the left eye that slides down one half-row
// between the two frames (fps 2 = a slow trickle)
const ERROR_SAD    = sadEyes(dbl(stamp(BODIES.droop, null)), 4, [6, 16]);
const ERROR_SAD_A  = tear(ERROR_SAD, 6, 7);
const ERROR_SAD_B  = tear(ERROR_SAD, 7, 7);
// squash dy 2 -> eyes at base row 4 -> doubled rows 8-9, slit on row 9
const SLEEP_CLOSED = slitEyes(dbl(stamp(BODIES.squash, null)), 9, [6, 16]);
// wake-up transition frames: eyes half-open on the risen body, a reach-up,
// then the BIG stretch — arms thrown up with a squint — before idle
const WAKE_SLIT       = slitEyes(dbl(stamp(BODIES.normal, null)), 5, [6, 16]);
const WAKE_STRETCH    = dbl(stamp(BODIES.stretch, FACES.normal));
const WAKE_BIGSTRETCH = slitEyes(dbl(stamp(BODIES.bounceUp, null)), 5, [6, 16]);
// falling-asleep frame: arms sag while the eyes are already heavy
const SLEEP_DROWSY    = slitEyes(dbl(stamp(BODIES.droop, null)), 5, [6, 16]);
// needs_input: the hop with happy ∩-arch eyes (the original done animation,
// restored). stretch dy -1 -> eye row 1 -> doubled rows 2-3; normal -> rows
// 4-5; squash dy 2 -> rows 8-9. HOP_ARMSUP is the slow both-arms raise.
const HOP_STRETCH = archEyes(dbl(stamp(BODIES.stretch, null)), 2, [6, 16]);
const HOP_NORMAL  = archEyes(dbl(stamp(BODIES.normal, null)), 4, [6, 16]);
const HOP_SQUASH  = archEyes(dbl(stamp(BODIES.squash, null)), 8, [6, 16]);
const HOP_ARMSUP  = archEyes(dbl(stamp(BODIES.bounceUp, null)), 4, [6, 16]);
const WAVE_UP     = archEyes(dbl(stamp(BODIES.waveUp, null)), 4, [6, 16]);

// --- working: measured from the user's definitive mirrored GIF
// (spikes/reference-material/working-mirrored.gif, 222x162, 47 frames
// @60-70ms, cell ~5.9px). res 2 half-cell frames, 26 cols x 24 rows,
// feet on row 21, rows 22-23 ground headroom (bottom-anchored).
// Choreography: wind-up dance at home (~1.3s: rest, crouch-bounce x2,
// arms-up, stretch, settle) -> leap LEFT -> type on the little laptop
// (3-pose cycle ~267ms/keystroke, laptop bouncing up a half-row before
// each strike) -> grab the laptop, rise with it, leap back, land.
// Home frames are 26 half-cols at dx 0; away frames are 32 half-cols at
// dx -28 (frame col 0 lands on canvas unit 4, keeping the laptop clear of
// the window edge), so keep
// (-dx - 4*leftmost-art-col) <= MOTION_ENVELOPE.left (currently 28). ---

const WORK_REST = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '....################......',
  '....################......',
  '....##EE########EE##......',
  '....##EE########EE##......',
  '########################..',
  '########################..',
  '########################..',
  '########################..',
  '....################......',
  '....################......',
  '....################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
// crouch-bounce: squashed, right arm reaching out, left arm low
const WORK_CROUCH_A = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '....################......',
  '....################......',
  '....####################..',
  '....####################..',
  '....##EE################..',
  '....##EE#######EEE######..',
  '.###################......',
  '.###################......',
  '.###################......',
  '.###################......',
  '....################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
const WORK_CROUCH_B = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '....################......',
  '....################......',
  '....####################..',
  '....####################..',
  '....##EE################..',
  '....##EE#######EEE######..',
  '....################......',
  '.###################......',
  '.###################......',
  '.###################......',
  '.###################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
// the outro's landing crouch: same pose, both eyes open (no wink)
const WORK_LANDHOME = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '....################......',
  '....################......',
  '....####################..',
  '....####################..',
  '....##EE########EE######..',
  '....##EE########EE######..',
  '....################......',
  '.###################......',
  '.###################......',
  '.###################......',
  '.###################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
// left arm swings up level with the eyes
const WORK_UP = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '....################......',
  '....################......',
  '######EE########EE##......',
  '######EE########EE##......',
  '########################..',
  '########################..',
  '.#######################..',
  '..######################..',
  '....####################..',
  '....################......',
  '....################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
// stretched tall, left arm thrown high
const WORK_STRETCH = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '####......................',
  '####################......',
  '####################......',
  '######EE########EE##......',
  '....##EE########EE##......',
  '....################......',
  '....####################..',
  '....####################..',
  '....####################..',
  '....####################..',
  '....################......',
  '....################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
// settling: arms come back down in three quick beats
const WORK_SET1 = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '....################......',
  '....################......',
  '....################......',
  '....##EE########EE##......',
  '....##EE########EE####....',
  '.#######################..',
  '.#######################..',
  '.#######################..',
  '.###################......',
  '####################......',
  '####################......',
  '####################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
const WORK_SET2 = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '....################......',
  '....################......',
  '....################......',
  '....##EE########EE##......',
  '....##EE########EE##......',
  '....####################..',
  '....####################..',
  '....####################..',
  '....####################..',
  '####################......',
  '####################......',
  '####################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
const WORK_SET3 = [
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '..........................',
  '....################......',
  '....################......',
  '....################......',
  '....##EE########EE######..',
  '....##EE########EE######..',
  '########################..',
  '########################..',
  '####################......',
  '....################......',
  '....################......',
  '....################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
// The away-from-home frames are 32 half-cols wide (dx -32) so the little
// grey laptop at the work spot fits. Laptop, measured pixel-exact from the
// GIF (3px = 1 half-cell): the screen is a diagonal TWO half-cells wide
// (back edge + display line), 5 bands from tip (row 16 col 0 at rest) down
// to the base's left end; the base is ONE half-row, 6 wide (cols 4-9 on
// row 21, same grey — the dark second row in old art was the platform, not
// the laptop). The laptop pops UP one half-row on HAM_A and on landing.
// Right side of the body is back-shaded '%', the near arm and back leg are
// dark, and the near eye (closest to the laptop) is solid black.

// airborne, arms thrown up, laptop waiting below
const WORK_LEAP = [
  '................................',
  '................................',
  '...........####......####.......',
  '...........####......####.......',
  '..........###############%%.....',
  '..........###############%%.....',
  '..........###############%%.....',
  '..........###############%%.....',
  '............#############%%.....',
  '............#EE########EE#%%....',
  '............#EE########EE#%%....',
  '............#############%%.....',
  '............#############%%.....',
  '............#############%%.....',
  '............#############%%.....',
  '............#############%%.....',
  'L...........#############%%.....',
  'LL..........#############%%.....',
  '.LL.........##..##....##..%%....',
  '..LL........##..##....##..%%....',
  '...LL.......##..##....##..%%....',
  '....LLLLLL..##..##....##..%%....',
  '................................',
  '................................',
];
// landing at the work spot, turning left — the impact bounces the laptop
// up one half-row (measured: GIF f17)
const WORK_LAND = [
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '........################........',
  '.......################%%.......',
  '.......################%%%%.....',
  '.......###%EE######EE##%%%%.....',
  '.......###%EE######EE##%%%%.....',
  '.......###%EE######EE##%%%%.....',
  '.......##%%%###########%%%%.....',
  '.......##%%%###########%%%%.....',
  'L..........############%%%%.....',
  'LL.....################%%%%.....',
  '.LL....################%%%%.....',
  '..LL...%%%%%%..##....##..%%.....',
  '...LL..%%%%%%..##....##..%%.....',
  '....LLLLLL.....###...###.%%%....',
  '...........###.###...###.%%%....',
  '................................',
  '................................',
];
// typing cycle, in measured play order: strike (B, 67ms) -> arm swings
// high (C, 133ms) -> hand hovers low over the keys with the laptop popped
// UP one half-row (A, 67ms) -> strike... The laptop bounce is A's up-frame.
const WORK_HAM_A = [
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........EE######EE##%%%%.....',
  '...........EE######EE##%%%%.....',
  '...........############%%%%.....',
  '........%%%############%%%%.....',
  'L......%###############%%%%.....',
  'LL.....%###############%%%%.....',
  '.LL....%###############%%%%.....',
  '..LL...%%%%############%%%%.....',
  '...LL......##..##....##..%%.....',
  '....LLLLLL.###.###...###.%%%....',
  '...........###.###...###.%%%....',
  '................................',
  '................................',
];
const WORK_HAM_B = [
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........EE######EE##%%%%.....',
  '...........EE######EE##%%%%.....',
  '........%##############%%%%.....',
  '........%##############%%%%.....',
  '........%##############%%%%.....',
  'L.......%##############%%%%.....',
  'LL......%##############%%%%.....',
  '.LL....%%%%############%%%%.....',
  '..LL...%%%%##..##....##..%%.....',
  '...LL..%%%%###.###...###.%%%....',
  '....LLLLLL.###.###...###.%%%....',
  '................................',
  '................................',
];
const WORK_HAM_C = [
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........EE######EE##%%%%.....',
  '...........EE######EE##%%%%.....',
  '.......%%%%############%%%%.....',
  '.......%%%%############%%%%.....',
  '.......%###############%%%%.....',
  'L......%###############%%%%.....',
  'LL.....%###############%%%%.....',
  '.LL....%%##############%%%%.....',
  '..LL....%####..##....##..%%.....',
  '...LL......###.###...###.%%%....',
  '....LLLLLL.###.###...###.%%%....',
  '................................',
  '................................',
];
// the last cycle before packing up: the far arm reaches around and grabs
// the laptop — orange hand flat over the keyboard (measured: GIF f39-f41)
const WORK_GRAB_B = [
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........EE######EE##%%%%.....',
  '...........EE######EE##%%%%.....',
  '........%##############%%%%.....',
  '........%##############%%%%.....',
  '........%##############%%%%.....',
  'L.......%##############%%%%.....',
  'LL......%##############%%%%.....',
  '.LL....%%%%############%%%%.....',
  '..LL...%%%%##..##....##..%%.....',
  '...LL..%%%%###.###...###.%%%....',
  '....LL%###%###.###...###.%%%....',
  '................................',
  '................................',
];
const WORK_GRAB_C = [
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........############%%%%.....',
  '...........EE######EE##%%%%.....',
  '...........EE######EE##%%%%.....',
  '.......%%%%############%%%%.....',
  '.......%%%%############%%%%.....',
  '.......%###############%%%%.....',
  'L......%###############%%%%.....',
  'LL.....%###############%%%%.....',
  '.LL....################%%%%.....',
  '..LL...######..##....##..%%.....',
  '...LL..#######.###...###.%%%....',
  '....LL%###%###.###...###.%%%....',
  '................................',
  '................................',
];
// rising from the work spot, scooping the laptop up
const WORK_RISE = [
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '..........###############%%.....',
  '.........###############%%%.....',
  '........###############%%%%.....',
  '........####EE######EE#%%%%.....',
  '........####EE######EE#%%%%.....',
  'L.......####EE######EE#%%%%.....',
  'LL......###############%%%%.....',
  '.LL.....###############%%%%.....',
  '..LL....###############%%%%.....',
  '...LL...###############%%%%.....',
  '....LLL.###############%%%%.....',
  '........%%%%%..##....##..%%.....',
  '........%%%%%..##....##..%%.....',
  '........%%%%%..###...###.%%%....',
  '...........###.###...###.%%%....',
  '................................',
  '................................',
];
// mid-leap back home, stretched wide, laptop under the arm
const WORK_LEAPBACK = [
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '................................',
  '.............###############....',
  '.............###############....',
  '.............###############....',
  '.............##########EE###....',
  '.............##########EE###....',
  '..........L################%%##.',
  '.........LL################%%##.',
  '.......LL##################%%##.',
  '......LL###################%%##.',
  '......L.#################%%%....',
  '......LL..###############%%%....',
  '.............###############....',
  '.............##..##....##..%%...',
  '.............##..##....##..%%...',
  '.............##..##....##..%%...',
  '.............##..##....##..%%...',
  '................................',
  '................................',
];

// --- needs_input: the checkered-flag wave, measured from the user's GIF
// (132 frames @30fps -> 9-pose loop, ~70ms/pose, ~630ms cycle). Half-cell
// art (res 2): frames are 26 half-cols x 32 half-rows = 13x16 cells.
// Layout: half-rows 0-13 flag/pole/raised-arm zone, 14-25 body, 26-29 legs,
// 30-31 ground headroom (bottom-anchored -> feet on the idle ground line).
// Body at half-cols 4-19 aligns exactly with the idle body (units 48-112).
// The buddy leans as it waves: per-step dx sways the frame +-4 units while
// the legs' lower half shifts one half-col the other way, so the feet stay
// planted on screen. Happy closed ∩-arch eyes are baked in at the base
// sprite's eye cells (char cols 3 & 8, row 1). ---

// body block, straight (poses A, E, F — no lean)
const FLAG_BODY_C = [
  '....################......',
  '....################......',
  '....##EE########EE##......',
  '....#E##E######E##E#......',
  '....################......',
  '####################......',
  '####################......',
  '####################......',
  '####################......',
  '....################......',
  '....################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '..........................',
  '..........................',
];
// leaning right (poses B, C, D — drawn with step dx +4): feet stay planted
const FLAG_BODY_R = [
  '....################......',
  '....################......',
  '....##EE########EE##......',
  '....#E##E######E##E#......',
  '....################......',
  '####################......',
  '####################......',
  '####################......',
  '####################......',
  '....################......',
  '....################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '...##..##....##..##.......',
  '...##..##....##..##.......',
  '..........................',
  '..........................',
];
// leaning left (poses G, H, I — drawn with step dx -4): left arm rides
// half a cell lower, feet stay planted
const FLAG_BODY_L = [
  '....################......',
  '....################......',
  '....##EE########EE##......',
  '....#E##E######E##E#......',
  '....################......',
  '....################......',
  '####################......',
  '####################......',
  '####################......',
  '####################......',
  '....################......',
  '....################......',
  '....##..##....##..##......',
  '....##..##....##..##......',
  '.....##..##....##..##.....',
  '.....##..##....##..##.....',
  '..........................',
  '..........................',
];

// flag zones (half-rows 0-13): raised right arm, tilting pole, waving cloth.
// Checkers alternate E/w on (row+col) parity inside each pose's cloth outline.
const FLAG_ZONE_A = [ // cloth hangs down-left of the pole
  '..........................',
  '..........................',
  '.............EwEwEE.......',
  '............EwEwEwE.......',
  '...........EwEwEwEE.......',
  '...........wEwEwEwE.......',
  '...........EwEwEwEE.......',
  '...........wEw....E.......',
  '...........EwE....E.......',
  '...........wE.....E.......',
  '..................E.......',
  '................####......',
  '................####......',
  '................####......',
];
const FLAG_ZONE_B = [ // gathering, pole tips right
  '..........................',
  '..........................',
  '..............wEwEwE......',
  '............EwEwEwEE......',
  '..........wEwEwEwEwE......',
  '..........EwEwEwEwEE......',
  '............wEwEwEwE......',
  '...........wEw.....E......',
  '...........Ew......E......',
  '...................E......',
  '...................E......',
  '.................####.....',
  '.................####.....',
  '.................####.....',
];
const FLAG_ZONE_C = [ // cloth spreads, lifting
  '............wEwEw.........',
  '..........EwEwEwEwEE......',
  '..........wEwEwEwEwE......',
  '..........EwEwEwEwEE......',
  '..........wEwEwEwEwE......',
  '...........wEwEwEwEE......',
  '.............EwEwE.E......',
  '...................E......',
  '...................E......',
  '...................E......',
  '...................E......',
  '.................####.....',
  '.................####.....',
  '.................####.....',
];
const FLAG_ZONE_D = [ // apex: arm punches half a cell higher, cloth up top
  '.............EwEwE........',
  '.............wEwEwEE......',
  '.............EwEwEwEw.....',
  '.............wEwEwEwE.....',
  '..............wEwEwEw.....',
  '................EwEwE.....',
  '...................E......',
  '...................E......',
  '...................E......',
  '...................E......',
  '.................####.....',
  '.................####.....',
  '.................####.....',
  '.................####.....',
];
const FLAG_ZONE_E = [ // swinging over to the right
  '..........................',
  '..................EwEw....',
  '..................EEwEw...',
  '..................EwEwE...',
  '..................EEwEwE..',
  '..................EwEwEw..',
  '..................E.wEwE..',
  '..................E.EwE...',
  '..................E.wE....',
  '..................E.......',
  '..................E.......',
  '.................####.....',
  '.................####.....',
  '.................####.....',
];
const FLAG_ZONE_F = [ // streaming right, curling down
  '..........................',
  '..........................',
  '.................EwEwE....',
  '.................EEwEwEwE.',
  '.................EwEwEwEw.',
  '.................EEwEwEwE.',
  '.................E.EwEwEw.',
  '.................E..EwEwE.',
  '.................E...EwEw.',
  '.................E....EwE.',
  '.................E........',
  '...............####.......',
  '...............####.......',
  '...............####.......',
];
const FLAG_ZONE_G = [ // full stream right
  '..........................',
  '..........................',
  '................EEwEw.....',
  '................EwEwEwE...',
  '................EEwEwEwEw.',
  '................EwEwEwEwEw',
  '................EEwEwEwEwE',
  '................E....wEwEw',
  '................E.....wEwE',
  '................E......wEw',
  '................E.........',
  '..............####........',
  '..............####........',
  '..............####........',
];
const FLAG_ZONE_H = [ // furthest extension
  '..........................',
  '..........................',
  '...............EwEwEw.....',
  '...............EEwEwEwEwE.',
  '...............EwEwEwEwEwE',
  '...............EEwEwEwEwEw',
  '...............E.EwEwEwEwE',
  '...............E....EwEwEw',
  '...............E.......Ew.',
  '...............E..........',
  '...............E..........',
  '..............####........',
  '..............####........',
  '..............####........',
];
const FLAG_ZONE_I = [ // whipping back over the pole
  '..........................',
  '..............EwEw........',
  '..............wEwEw.......',
  '................EwEwEw....',
  '................wEwEwEw...',
  '................EwEwEwE...',
  '................EEwEwEw...',
  '................EwEwEw....',
  '................E.wEw.....',
  '................E.........',
  '................E.........',
  '...............####.......',
  '...............####.......',
  '...............####.......',
];

const flagFrame = (zone, body) => [...zone, ...body];
const FLAG_A = flagFrame(FLAG_ZONE_A, FLAG_BODY_C);
const FLAG_B = flagFrame(FLAG_ZONE_B, FLAG_BODY_R);
const FLAG_C = flagFrame(FLAG_ZONE_C, FLAG_BODY_R);
const FLAG_D = flagFrame(FLAG_ZONE_D, FLAG_BODY_R);
const FLAG_E = flagFrame(FLAG_ZONE_E, FLAG_BODY_C);
const FLAG_F = flagFrame(FLAG_ZONE_F, FLAG_BODY_C);
const FLAG_G = flagFrame(FLAG_ZONE_G, FLAG_BODY_L);
const FLAG_H = flagFrame(FLAG_ZONE_H, FLAG_BODY_L);
const FLAG_I = flagFrame(FLAG_ZONE_I, FLAG_BODY_L);

// --- soccer: the ball juggle (demo-panel only; a candidate seasonal
// variation — it no longer auto-plays during idle). Motion is MEASURED from the
// user's 13-frame reference GIF (recovered from the session transcript,
// ~330ms/frame, ball path + poses transcribed pixel-by-pixel); bodies are
// HAND-AUTHORED clean chunky poses shaped by that transcription — defined
// snouts on the leans/kick, tip-toe headers, crisp legs — per the style
// law (extraction edges read as ragged; measured motion + clean art wins).
// The cycle runs at 167ms with ball in-between positions on the half-beats
// (the body changes on the measured 333ms beat, the ball arcs smoothly).
// Ball: canonical 6-wide round big-2x2-patch disc, rotation phase advances
// every step. Frames 34x26 half-cells, body block at row 6 / col 5, every
// step dx -20 to stay planted on the rest spot. ---

// clean pose bodies (20 rows x 24 half-cols, dbl-grid layout)
const SOCP_KICK = [
  '........................',
  '........................',
  '......################..',
  '......################..',
  '......##EE########EE##..',
  '......##EE########EE##..',
  '####################....',
  '####################....',
  '....##################..',
  '....##################..',
  '....##################..',
  '....##################..',
  '....##################..',
  '....####################',
  '.....##..##...##......##',
  '.....##..##...##......##',
  '.....##..##.............',
  '.....##..##.............',
  '........................',
  '........................',
];
const SOCP_LEAN_L = [
  '........................',
  '........................',
  '..################......',
  '..################......',
  '..#EE########EE###......',
  '..#EE########EE###......',
  '#####################...',
  '#####################...',
  '....####################',
  '....####################',
  '.....###############....',
  '.....###############....',
  '.....###############....',
  '.....###############....',
  '...##....##..##...##....',
  '...##....##..##...##....',
  '.............##...##....',
  '.............##...##....',
  '........................',
  '........................',
];
const SOCP_LEAN_R = mirrorPose(SOCP_LEAN_L);
const SOCP_SETTLE = [
  '........................',
  '........................',
  '........................',
  '....################....',
  '....################....',
  '....##EE########EE##....',
  '....##EE########EE##....',
  '########################',
  '########################',
  '########################',
  '########################',
  '....################....',
  '....################....',
  '....################....',
  '....################....',
  '....##..##....##..##....',
  '....##..##....##..##....',
  '....##..##....##..##....',
  '........................',
  '........................',
];
const SOCP_TRACK_R = dbl(stamp(BODIES.normal, FACES.side));
const SOCP_TRACK_L = dbl(stamp(BODIES.normal, FACES.sideL));
const SOCP_REST    = dbl(stamp(BODIES.normal, FACES.normal));
const SOCP_HEADER = (() => {
  const g = dbl(stamp(BODIES.normal, FACES.up)).map((r) => r.split(''));
  for (const c of [14, 15, 18, 19]) { g[16][c] = '.'; g[17][c] = '.'; }
  return g.map((r) => r.join(''));
})();

const SOC_BALL_MASK = ['.####.', '######', '######', '######', '######', '.####.'];
const SOC_W = 34, SOC_H = 26;
const socFrame = (block, ballTop, ballLeft, phase) => {
  const rows = Array.from({ length: SOC_H }, () => Array(SOC_W).fill('.'));
  block.forEach((br, r) => {
    for (let c = 0; c < br.length; c++) if (br[c] !== '.') rows[6 + r][5 + c] = br[c];
  });
  if (ballTop !== null) {
    SOC_BALL_MASK.forEach((br, r) => {
      for (let c = 0; c < br.length; c++) {
        if (br[c] === '.') continue;
        const R = ballTop + r, C = ballLeft + c;
        if (R < 0 || R >= SOC_H || C < 0 || C >= SOC_W) continue;
        rows[R][C] = ((Math.floor(r / 2) + Math.floor(c / 2) + phase) % 2) ? 'E' : 'w';
      }
    });
  }
  return rows.map((r) => r.join(''));
};

// measured keys (ball top/left/rotation phase) + arc midpoints on half-beats
const SOCCER = {
  intro: [], // the reference starts mid-play — the kick-up is the opener
  cycle: [
    { frame: socFrame(SOCP_KICK, 15, 28, 1),    dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_KICK, 10, 26, 0),    dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_TRACK_R, 8, 25, 0),  dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_TRACK_R, 4, 20, 1),  dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_HEADER, 2, 16, 0),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_HEADER, 2, 10, 1),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_TRACK_L, 4, 4, 1),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_TRACK_L, 6, 2, 0),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_TRACK_L, 8, 1, 1),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_TRACK_L, 9, 3, 0),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_LEAN_L, 11, 5, 0),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_LEAN_L, 5, 10, 1),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_HEADER, 1, 16, 0),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_HEADER, 5, 22, 1),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_LEAN_R, 10, 27, 0),  dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_LEAN_R, 9, 25, 1),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_LEAN_R, 10, 24, 0),  dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_LEAN_R, 4, 18, 1),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_HEADER, 1, 13, 1),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_HEADER, 5, 8, 0),    dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_LEAN_L, 10, 3, 0),   dx: -20, dy: 0, ms: 167 },
    { frame: socFrame(SOCP_LEAN_L, 13, 15, 1),  dx: -20, dy: 0, ms: 167 },
  ],
  outro: [
    { frame: socFrame(SOCP_SETTLE, null, 0, 0), dx: -20, dy: 0, ms: 333 },
    { frame: socFrame(SOCP_REST, null, 0, 0),   dx: -20, dy: 0, ms: 500 },
  ],
};

// frames: composed at load time. motion: engine-level transform (integer px).
// overlay: glyph animation rendered alongside. No clip uses color for alarm —
// attention is carried by pose, motion, and soft overlays (cuteness mandate).
const CLIPS = {
  // staged: falling asleep and waking both play transitions. The intro is
  // the buddy making itself comfortable — heavy eyes, sagging arms, sinking
  // down (step dy offsets counter the droop motion's constant +4 so the
  // sink is gradual), then two little scootches to settle into the spot;
  // the dream cloud only appears once it's settled (pet.js delays the
  // overlay by the intro's duration). The outro is the wake-up: rise,
  // eyes open, reach up, BIG arms-thrown-up stretch with a squint, ease
  // down, settle. Outros render without motion; the cloud lifts and poofs
  // on the outro clock early enough to clear the raised arms.
  // 60Hz-aligned step ms.
  sleeping: {
    palette: 'normal', motion: 'droop', overlay: 'dream', res: 2,
    stages: {
      intro: [
        { frame: IDLE_OPEN,    dx: 0,  dy: -4, ms: 133 }, // standing beat
        { frame: WAKE_SLIT,    dx: 0,  dy: -4, ms: 200 }, // eyes grow heavy
        { frame: SLEEP_DROWSY, dx: 0,  dy: -3, ms: 200 }, // arms sag, sinking
        { frame: SLEEP_CLOSED, dx: 0,  dy: -2, ms: 133 }, // settling down
        { frame: SLEEP_CLOSED, dx: -2, dy: 0,  ms: 133 }, // comfy scootch...
        { frame: SLEEP_CLOSED, dx: 2,  dy: 0,  ms: 133 }, // ...and back
      ],
      loop: [{ frame: SLEEP_CLOSED, dx: 0, dy: 0, ms: 1000 }],
      outro: [
        { frame: SLEEP_CLOSED,    dx: 0, dy: 4, ms: 200 }, // stir
        { frame: SLEEP_CLOSED,    dx: 0, dy: 2, ms: 67 },  // rising
        { frame: WAKE_SLIT,       dx: 0, dy: 0, ms: 133 }, // eyes half open
        { frame: IDLE_OPEN,       dx: 0, dy: 0, ms: 67 },  // eyes open
        { frame: WAKE_STRETCH,    dx: 0, dy: 0, ms: 133 }, // reaching up
        { frame: WAKE_BIGSTRETCH, dx: 0, dy: 0, ms: 600 }, // arms up + squint
        { frame: WAKE_STRETCH,    dx: 0, dy: 0, ms: 133 }, // easing down
        { frame: IDLE_OPEN,       dx: 0, dy: 0, ms: 133 }, // settle
      ],
    },
  },
  // glances go BOTH ways but RARELY (~12s apart in a ~25s cycle) — mostly
  // calm forward gazing with scattered blinks
  idle: {
    fps: 2, palette: 'normal', motion: 'breathe', overlay: null, res: 2,
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
  // Staged clip — the user's definitive mirrored GIF: wind-up dance at
  // home (plays once), leap LEFT, hammer at the ground, and on state exit
  // rise, leap back home and land. Step ms/dx measured from the GIF.
  working: {
    palette: 'normal', overlay: null, motion: 'none', res: 2,
    stages: {
      // Step durations are multiples of ~16.7ms (60Hz display frames):
      // misaligned durations render as alternating 5/6-frame steps, which
      // reads as stutter in the tight typing loop.
      intro: [
        { frame: WORK_REST,     dx: 0,  dy: 0, ms: 200 },
        { frame: WORK_CROUCH_A, dx: 0,  dy: 0, ms: 67 },
        { frame: WORK_CROUCH_B, dx: 0,  dy: 0, ms: 67 },
        { frame: WORK_CROUCH_A, dx: 0,  dy: 0, ms: 133 },
        { frame: WORK_CROUCH_B, dx: 0,  dy: 0, ms: 133 },
        { frame: WORK_UP,       dx: 0,  dy: 0, ms: 133 },
        { frame: WORK_STRETCH,  dx: 0,  dy: 0, ms: 133 },
        { frame: WORK_SET1,     dx: 0,  dy: 0, ms: 67 },
        { frame: WORK_SET2,     dx: 0,  dy: 0, ms: 133 },
        { frame: WORK_SET3,     dx: 0,  dy: 0, ms: 67 },
        { frame: WORK_LEAP,     dx: -28, dy: 0, ms: 67 },
        { frame: WORK_LAND,     dx: -28, dy: 0, ms: 67 },
      ],
      // measured keystroke cycle (~267ms): strike -> arm swings high
      // (long beat) -> hand hovers low with the laptop bounced up -> strike
      loop: [
        { frame: WORK_HAM_B, dx: -28, dy: 0, ms: 67 },
        { frame: WORK_HAM_C, dx: -28, dy: 0, ms: 133 },
        { frame: WORK_HAM_A, dx: -28, dy: 0, ms: 67 },
      ],
      outro: [
        { frame: WORK_GRAB_B,   dx: -28, dy: 0, ms: 133 },
        { frame: WORK_GRAB_C,   dx: -28, dy: 0, ms: 67 },
        { frame: WORK_RISE,     dx: -28, dy: 0, ms: 67 },
        { frame: WORK_LEAPBACK, dx: -28, dy: 0, ms: 67 },
        { frame: WORK_LANDHOME, dx: 0,   dy: 0, ms: 200 },
      ],
    },
  },
  thinking: {
    fps: 2, palette: 'normal', motion: 'tilt', overlay: 'dots',
    frames: [stamp(BODIES.normal, FACES.up), stamp(BODIES.normal, FACES.up)],
  },
  // the hop + still happy pose (the original done animation, restored),
  // with BOTH arms slowly rising, holding and lowering every so often. The
  // tail is a long pre-baked irregular loop (~8.5s at fps 6): arm raises at
  // uneven offsets (~1.2s, ~3.3s, ~6.3s) read as semi-random without any
  // engine randomness (user: slow, both together, semi-random intervals).
  needs_input: {
    fps: 6, palette: 'normal', motion: 'hopOnce', overlay: null, res: 2,
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
  // the checkered-flag wave, measured from the user's GIF: 9 poses at 67ms
  // (the GIF's ~70ms snapped to the 60Hz grid — raw 70ms renders as
  // alternating 5/6-display-frame steps, which reads as choppy; user caught
  // it), feet planted (no hop) — the finish-line flag says "done".
  // res 2 = half-cell art.
  done: {
    palette: 'normal', overlay: null, motion: 'none', res: 2,
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
  // very sad rain-cloud droop: sagging body, closed ∪ eyes, a slow tear,
  // gentle breathing, grey cloud + falling drops. pet.js adds render-level
  // GLITCHES at random intervals while in this state.
  error: {
    fps: 2, palette: 'normal', motion: 'breathe', overlay: 'rain', res: 2,
    frames: [ERROR_SAD_A, ERROR_SAD_B],
  },
  // the ball juggle, forceable from the demo panel only (it no longer
  // auto-plays during idle) — kept as a candidate seasonal variation
  soccer: {
    palette: 'normal', overlay: null, motion: 'none', res: 2,
    stages: {
      intro: SOCCER.intro,
      loop: SOCCER.cycle,
      outro: SOCCER.outro,
    },
  },
  blind: {
    fps: 1, palette: 'grey', motion: 'droop', overlay: 'quest', res: 2,
    frames: [IDLE_OPEN, IDLE_BLINK],
  },
  // hello wave: the needs_input arm raise (the motion the user approved),
  // ONE-armed per the user — the right arm pops between its side nub and
  // the raised bar with the same ∩ smile-eyes, greeting tempo. Played by
  // the host at startup (phase 2: on SessionStart), which returns to idle
  // when the greeting is over. Staged so leaving the state can't cut the
  // gesture mid-swing: the outro settles on the arm-down pose for a beat
  // before the next state shows (the host also times its handoff into a
  // down-frame window). 250ms = 15 display frames at 60Hz.
  wave: {
    palette: 'normal', motion: 'none', overlay: null, res: 2,
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

const SPRITE_W = 12;
const SPRITE_H = 10;

validateClips(CLIPS, SPRITE_W, SPRITE_H);

// The measured away-from-home working poses (turned body, back-shade,
// laptop) double as the pose source for the CLI style, which re-projects
// them onto its own body proportions (sprites-cli.js). Same for the flag
// zones (pole + cloth are body-independent).
const WORK_AWAY = {
  LEAP: WORK_LEAP, LAND: WORK_LAND, HAM_A: WORK_HAM_A, HAM_B: WORK_HAM_B, HAM_C: WORK_HAM_C,
  GRAB_B: WORK_GRAB_B, GRAB_C: WORK_GRAB_C, RISE: WORK_RISE, LEAPBACK: WORK_LEAPBACK,
};
const FLAG_ZONES = {
  A: FLAG_ZONE_A, B: FLAG_ZONE_B, C: FLAG_ZONE_C, D: FLAG_ZONE_D, E: FLAG_ZONE_E,
  F: FLAG_ZONE_F, G: FLAG_ZONE_G, H: FLAG_ZONE_H, I: FLAG_ZONE_I,
};

export { CLIPS, PALETTES, GLYPHS, SPRITE_W, SPRITE_H, SOCCER, WORK_AWAY, FLAG_ZONES };
