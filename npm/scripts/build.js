'use strict';
/**
 * Assemble the npm package from the Rust project (maintainers only).
 *
 *   node npm/scripts/build.js              build the dist binary + assemble
 *   node npm/scripts/build.js --skip-build reuse the existing target/dist
 *
 * It keeps `package.json` in step with `Cargo.toml` (version + binary
 * checksum), copies the binary into `vendor/` and the repo's ExtraResources
 * into the package. Runs automatically before `npm pack` / `npm publish`.
 */

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');
const { spawnSync } = require('child_process');

const PKG_DIR = path.join(__dirname, '..');
const REPO = path.join(PKG_DIR, '..');
const EXE = 'beyond-cli.exe';

function fail(msg) {
  console.error(`build: ${msg}`);
  process.exit(1);
}

// 1. version comes from Cargo.toml — one source of truth
const cargoToml = fs.readFileSync(path.join(REPO, 'Cargo.toml'), 'utf8');
const version = (cargoToml.match(/^version\s*=\s*"([^"]+)"/m) || [])[1];
if (!version) fail('could not read the version from Cargo.toml');

// 2. release binary
if (!process.argv.includes('--skip-build')) {
  // `dist` is the size-optimised profile in Cargo.toml (plain --release is
  // tuned for fast rebuilds while developing).
  console.log('build: cargo build --profile dist');
  const cargo = spawnSync('cargo', ['build', '--profile', 'dist'], { cwd: REPO, stdio: 'inherit' });
  if (cargo.status !== 0) fail('cargo build failed');
}
const built = path.join(REPO, 'target', 'dist', EXE);
if (!fs.existsSync(built)) fail(`${built} not found — run without --skip-build`);

// 3. vendor/ — the binary that ships inside the package
const vendor = path.join(PKG_DIR, 'vendor');
fs.mkdirSync(vendor, { recursive: true });
fs.copyFileSync(built, path.join(vendor, EXE));
const sha256 = crypto.createHash('sha256').update(fs.readFileSync(built)).digest('hex');

// 4. ExtraResources — bundled MCPs and Skills (fresh copy each build)
const resources = path.join(PKG_DIR, 'ExtraResources');
fs.rmSync(resources, { recursive: true, force: true });
fs.cpSync(path.join(REPO, 'ExtraResources'), resources, { recursive: true });
fs.copyFileSync(path.join(REPO, 'LICENSE'), path.join(PKG_DIR, 'LICENSE'));

// 5. package.json — version + checksum of exactly this binary
const pkgFile = path.join(PKG_DIR, 'package.json');
const pkg = JSON.parse(fs.readFileSync(pkgFile, 'utf8'));
pkg.version = version;
pkg.beyondBinary.sha256 = sha256;
fs.writeFileSync(pkgFile, JSON.stringify(pkg, null, 2) + '\n');

const mb = (fs.statSync(built).size / 1048576).toFixed(1);
console.log(`build: beyond-cli v${version} assembled (${mb} MB, sha256 ${sha256.slice(0, 12)}…)`);
console.log(`build: for a GitHub release, upload vendor/${EXE} as ${pkg.beyondBinary.asset}`);
