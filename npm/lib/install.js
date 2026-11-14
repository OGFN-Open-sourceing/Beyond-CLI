'use strict';
/**
 * Beyond CLI installer — shared by the `postinstall` hook and the launcher.
 *
 * The package normally ships the native binary in `vendor/`. pnpm and bun do
 * not run install scripts by default, so nothing here may be *required* at
 * install time: the launcher calls `ensureBinary()` again on first run and
 * finishes whatever the hook could not do.
 *
 * Steps (the "MSI" part):
 *   1. locate the binary — bundled, already downloaded, or fetch the release
 *   2. verify a download against the SHA-256 recorded at build time
 *   3. copy bundled ExtraResources (MCPs, Skills) into the user's data folder,
 *      never overwriting files the user already has
 */

const fs = require('fs');
const os = require('os');
const path = require('path');
const crypto = require('crypto');
const https = require('https');

const pkg = require('../package.json');

const ROOT = path.join(__dirname, '..');
const EXE = 'beyond-cli.exe';
const RELEASES = 'https://github.com/OGFN-Open-sourceing/Beyond-CLI/releases/download';
const MAX_REDIRECTS = 5;

/** `%LOCALAPPDATA%\beyond-cli` — the same folder the app itself uses. */
function dataDir() {
  const local = process.env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local');
  return path.join(local, 'beyond-cli');
}

const bundledBinary = () => path.join(ROOT, 'vendor', EXE);
const downloadedBinary = () => path.join(dataDir(), 'bin', pkg.version, EXE);

function sha256(file) {
  return crypto.createHash('sha256').update(fs.readFileSync(file)).digest('hex');
}

/** GET `url` into `dest`, following redirects (GitHub serves assets via one). */
function download(url, dest, redirects = 0) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { 'user-agent': 'beyond-cli-installer' } }, (res) => {
        const { statusCode, headers } = res;
        if (statusCode >= 300 && statusCode < 400 && headers.location) {
          res.resume();
          if (redirects >= MAX_REDIRECTS) return reject(new Error('too many redirects'));
          const next = new URL(headers.location, url);
          if (next.protocol !== 'https:') return reject(new Error('refusing a non-https redirect'));
          return resolve(download(next.href, dest, redirects + 1));
        }
        if (statusCode !== 200) {
          res.resume();
          return reject(new Error(`download failed (HTTP ${statusCode}) — ${url}`));
        }
        const out = fs.createWriteStream(dest);
        res.pipe(out);
        out.on('finish', () => out.close(resolve));
        out.on('error', reject);
      })
      .on('error', reject);
  });
}

/**
 * Return the path of a runnable binary, downloading it if necessary.
 * `log` receives one-line progress messages.
 */
async function ensureBinary(log = () => {}) {
  if (process.platform !== 'win32') {
    throw new Error(`Beyond CLI currently supports Windows only (this is ${process.platform})`);
  }
  // escape hatch for development: point at a local cargo build
  const override = process.env.BEYOND_CLI_BINARY;
  if (override) {
    if (!fs.existsSync(override)) throw new Error(`BEYOND_CLI_BINARY points at a missing file: ${override}`);
    return override;
  }
  if (fs.existsSync(bundledBinary())) return bundledBinary();
  if (fs.existsSync(downloadedBinary())) return downloadedBinary();

  // Slim package: fetch the release asset for this exact version.
  const { asset, sha256: expected } = pkg.beyondBinary || {};
  if (!asset || !expected) {
    throw new Error('this package has no bundled binary and no checksum to verify a download with — reinstall beyond-cli');
  }
  const url = `${RELEASES}/v${pkg.version}/${asset}`;
  const dest = downloadedBinary();
  const tmp = `${dest}.${process.pid}.tmp`;
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  log(`downloading Beyond CLI v${pkg.version}`);
  try {
    await download(url, tmp);
    const actual = sha256(tmp);
    if (actual !== expected) {
      throw new Error(`checksum mismatch for ${asset} — expected ${expected}, got ${actual}`);
    }
    fs.renameSync(tmp, dest);
  } finally {
    fs.rmSync(tmp, { force: true });
  }
  log('download verified');
  return dest;
}

/**
 * Copy the package's ExtraResources into the user's data folder.
 * Existing files are left alone, so user edits and their own MCPs survive
 * upgrades. Returns how many files were added.
 */
function seedResources() {
  const src = path.join(ROOT, 'ExtraResources');
  if (!fs.existsSync(src)) return 0;
  const dest = path.join(dataDir(), 'ExtraResources');
  let added = 0;
  const walk = (from, to) => {
    fs.mkdirSync(to, { recursive: true });
    for (const entry of fs.readdirSync(from, { withFileTypes: true })) {
      if (entry.name === '.gitkeep') continue;
      const a = path.join(from, entry.name);
      const b = path.join(to, entry.name);
      if (entry.isDirectory()) walk(a, b);
      else if (entry.isFile() && !fs.existsSync(b)) {
        fs.copyFileSync(a, b);
        added += 1;
      }
    }
  };
  walk(src, dest);
  return added;
}

// ------------------------------------------------------------ postinstall

/** Paint `text` with the Beyond violet → aqua gradient (truecolor). */
function gradient(text) {
  if (!process.stdout.isTTY || process.env.NO_COLOR) return text;
  const stops = [[124, 58, 237], [217, 70, 239], [96, 165, 250], [45, 212, 191]];
  const chars = [...text];
  return (
    chars
      .map((ch, i) => {
        const t = (chars.length > 1 ? i / (chars.length - 1) : 0) * (stops.length - 1);
        const k = Math.min(Math.floor(t), stops.length - 2);
        const [a, b] = [stops[k], stops[k + 1]];
        const c = a.map((v, j) => Math.round(v + (b[j] - v) * (t - k)));
        return `\x1b[38;2;${c[0]};${c[1]};${c[2]}m${ch}`;
      })
      .join('') + '\x1b[0m'
  );
}

async function main() {
  const step = (msg) => console.log(`  ✓ ${msg}`);
  console.log(`\n  ${gradient('✦ B E Y O N D   C L I')}  v${pkg.version}\n`);
  try {
    const exe = await ensureBinary(step);
    step(`binary ready — ${exe}`);
    const added = seedResources();
    step(added ? `added ${added} bundled resource file(s)` : 'ExtraResources up to date');
    step(`MCPs and Skills live in ${path.join(dataDir(), 'ExtraResources')}`);
    console.log(`\n  run ${gradient('beyond')} to start\n`);
  } catch (err) {
    // Never fail the package install: the launcher retries on first run.
    console.log(`  ! setup will finish on first launch (${err.message})\n`);
  }
}

if (require.main === module) main();

module.exports = { ensureBinary, seedResources, dataDir };
