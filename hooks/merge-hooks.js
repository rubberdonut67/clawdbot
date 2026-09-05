// Phase 0 spike: merge the Claw'dbot hook block into ~/.claude/settings.json.
// Backs up the original to settings.json.clawdbot-backup first.
// Aborts if a hooks key already exists (never clobber user hooks).
const fs = require('fs');
const os = require('os');
const path = require('path');

const file = path.join(os.homedir(), '.claude', 'settings.json');
const backup = file + '.clawdbot-backup';

const EVENTS = [
  'SessionStart', 'SessionEnd', 'UserPromptSubmit',
  'PreToolUse', 'PostToolUse', 'PostToolUseFailure',
  'PermissionRequest', 'Elicitation', 'Notification',
  'Stop', 'StopFailure',
];

const raw = fs.readFileSync(file, 'utf8');
if (!fs.existsSync(backup)) fs.writeFileSync(backup, raw);

const cfg = JSON.parse(raw);
if (cfg.hooks) {
  console.error('settings.json already has a hooks key — aborting');
  process.exit(1);
}

cfg.hooks = Object.fromEntries(EVENTS.map((e) => [e, [{
  hooks: [{ type: 'http', url: 'http://127.0.0.1:4317/event', timeout: 2 }],
}]]));

fs.writeFileSync(file, JSON.stringify(cfg, null, 2) + '\n');
console.log('merged hooks into ' + file);
console.log('backup at ' + backup);
