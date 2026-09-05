// Phase 0 spike: remove only the Claw'dbot hook entries (URL on port 4317)
// from ~/.claude/settings.json, leaving everything else untouched.
const fs = require('fs');
const os = require('os');
const path = require('path');

const file = path.join(os.homedir(), '.claude', 'settings.json');
const cfg = JSON.parse(fs.readFileSync(file, 'utf8'));

if (cfg.hooks) {
  for (const [event, groups] of Object.entries(cfg.hooks)) {
    const kept = groups.filter((g) =>
      !(g.hooks || []).every((h) => h.type === 'http' && String(h.url).includes('127.0.0.1:4317')));
    if (kept.length) cfg.hooks[event] = kept;
    else delete cfg.hooks[event];
  }
  if (Object.keys(cfg.hooks).length === 0) delete cfg.hooks;
}

fs.writeFileSync(file, JSON.stringify(cfg, null, 2) + '\n');
console.log('removed claw'dbot hooks from ' + file);
