#!/usr/bin/env node
'use strict';
/**
 * `beyond` — thin launcher for the native Beyond CLI binary.
 *
 * Finds (or, for a slim package, downloads and verifies) the binary, makes
 * sure bundled ExtraResources are in place, then hands the terminal over.
 * All arguments pass straight through: `beyond --theme galaxy`, `beyond setup`.
 */

const { spawnSync } = require('child_process');
const { ensureBinary, seedResources } = require('../lib/install');

(async () => {
  let exe;
  try {
    exe = await ensureBinary((msg) => console.error(`beyond: ${msg}`));
    seedResources();
  } catch (err) {
    console.error(`beyond: ${err.message}`);
    process.exit(1);
  }

  // The app owns the terminal (raw mode, alternate screen), so inherit stdio
  // and simply mirror its exit code.
  const result = spawnSync(exe, process.argv.slice(2), { stdio: 'inherit' });
  if (result.error) {
    console.error(`beyond: could not start ${exe} — ${result.error.message}`);
    process.exit(1);
  }
  process.exit(result.status ?? 1);
})();
