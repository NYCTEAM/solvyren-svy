import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cp, mkdir, readFile, readdir, realpath, writeFile } from 'node:fs/promises';
import { basename, delimiter, dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';

const root = await realpath(fileURLToPath(new URL('../', import.meta.url)));
const release = 'mainnet-retarget-24121';
const windows = process.platform === 'win32';
if (process.arch !== 'x64' || (!windows && process.platform !== 'linux')) {
  throw Error('Mainnet release tooling supports Windows and Linux x86-64.');
}
const executable = windows ? 'solvyren-node.exe' : 'solvyren-node';
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const rawDigest = '669568cfc22ef92da64eae6ae15b3116107f9ee58b751cd1cb886ec98291cc16';
const releaseFeatures = ['--no-default-features', '--features', 'mainnet-release'];

const privatePrefixes = [...new Set([
  dirname(dirname(root)),
  dirname(root),
  root,
  ...(process.env.SOLVYREN_REMAP_PREFIXES ?? '').split(delimiter).filter(Boolean),
].filter(prefix => prefix.length > 4))];

function audit(bytes) {
  const labels = [
    ['internal test chain tooling', /(?:pow-mx10-n16-local|pow-mx10-n16-publicdev|--mx10-author-output|eagw_pow_[a-z0-9_]+_local)/i],
    ['legacy author configuration', /EAGW_POW_(?:FINALITY_ONLY|AUTHORING_LIMIT|SESSION_MAX_SECONDS|SESSION_MAX_ATTEMPTS|GPU_UUID)/],
    ['development mnemonic', Buffer.from('626f74746f6d206472697665206f626579206c616b65206375727461696e20736d6f6b65206261736b657420686f6c642072616365206c6f6e656c79206669742077616c6b','hex')],
    ['old debugger database', /eagle_node\.pdb/i],
  ];
  const text = bytes.toString('latin1');
  const findings = [];
  for (const [label, pattern] of labels) {
    if (Buffer.isBuffer(pattern) ? bytes.includes(pattern) : pattern.test(text)) {
      findings.push(label);
    }
  }
  if (privatePrefixes.some(prefix => {
    const slash = prefix.replaceAll('\\', '/');
    const wslPath = /^[A-Za-z]:\//.test(slash) ? `/mnt/${slash[0].toLowerCase()}${slash.slice(2)}` : slash;
    return [prefix, slash, wslPath].some(path => text.includes(path) || bytes.includes(Buffer.from(path, 'utf16le')));
  })) {
    findings.push('absolute build path');
  }
  if (findings.length) throw Error(`Release audit failed: ${findings.join(', ')}`);
}

async function stageSdkWithoutDefaultDevelopmentSeed(target) {
  const metadata = spawnSync('cargo', ['metadata', '--locked', '--format-version', '1', ...releaseFeatures], {
    cwd: resolve(root, 'chain-core', 'node'),
    encoding: 'utf8', maxBuffer: 32 * 1024 * 1024,
    stdio: ['ignore', 'pipe', 'inherit'],
  });
  if (metadata.error || metadata.status !== 0) throw metadata.error ?? Error('Could not resolve the pinned SDK source.');
  const packages = JSON.parse(metadata.stdout).packages;
  const core = packages.find(item => item.name === 'sp-core' && item.version === '38.1.0');
  if (!core?.source?.startsWith('registry+')) throw Error('Expected registry SDK sp-core 38.1.0.');
  const stage = resolve(target, 'solvyren-release-sdk', 'sp-core-38.1.0');
  const original = dirname(core.manifest_path);
  const originalCrypto = await readFile(resolve(original, 'src', 'crypto.rs'), 'utf8');
  const fallback = 'let phrase = cap.phrase.unwrap_or(DEV_PHRASE);';
  if (originalCrypto.split(fallback).length !== 2) throw Error('Unexpected SDK secret URI parser.');
  // A release secret URI must explicitly name the seed. Signature verification
  // and explicitly supplied keys use the unchanged upstream implementation.
  const patched = originalCrypto.replace(
    fallback, 'let phrase = cap.phrase.ok_or(SecretStringError::InvalidPhrase)?;',
  );
  const stagedCrypto = resolve(stage, 'src', 'crypto.rs');
  if (await readFile(stagedCrypto, 'utf8').catch(error => {
    if (error.code !== 'ENOENT') throw error;
    return null;
  }) !== patched) {
    await mkdir(dirname(stage), { recursive: true });
    await cp(original, stage, { recursive: true });
    await writeFile(stagedCrypto, patched);
  }
  return stage;
}

if (process.argv[2] === '--audit' && process.argv[3]) {
  audit(await readFile(process.argv[3]));
  console.log('Binary content audit passed.');
} else {
  const output = resolve(root, 'release-local', release);
  const target = resolve(process.env.CARGO_TARGET_DIR ?? resolve(root, 'target'));
  const sdk = await stageSdkWithoutDefaultDevelopmentSeed(target);
  const flags = process.env.CARGO_ENCODED_RUSTFLAGS?.split('\x1f').filter(Boolean) ?? [];
  const remapPrefixes = [...new Set([...privatePrefixes, target])].sort((a, b) => a.length - b.length);
  for (const [index, prefix] of remapPrefixes.entries()) {
    flags.push(`--remap-path-prefix=${prefix}=/src/build-${index}`);
  }
  const build = spawnSync('cargo', [
    'rustc','--locked','--release','--config',`paths=[${JSON.stringify(sdk.replaceAll('\\', '/'))}]`,
    '-p','solvyren-node','--bin','solvyren-node',...releaseFeatures,'--','-C','strip=debuginfo',
  ], {
    cwd: root, stdio: 'inherit',
    env: {...process.env, SKIP_WASM_BUILD:'1', CARGO_TARGET_DIR:target, CARGO_ENCODED_RUSTFLAGS:flags.join('\x1f')},
  });
  if (build.error || build.status !== 0) throw build.error ?? Error('Mainnet build failed.');
  const path = resolve(target,'release', executable);
  const version = spawnSync(path,['--version'],{encoding:'utf8'});
  if (version.status !== 0 || !version.stdout.includes(release)) throw Error('Unexpected release version.');
  const bytes = await readFile(path);
  audit(bytes);
  const raw = await readFile(resolve(root,'network','mainnet.raw.json'));
  if (digest(raw) !== rawDigest) throw Error('Unexpected mainnet raw spec hash.');
  await mkdir(output,{recursive:true});
  const asset = windows ? 'solvyren-node-windows-x86_64.exe' : 'solvyren-node-linux-x86_64.gz';
  await writeFile(resolve(output,asset),windows ? bytes : gzipSync(bytes,{level:9}));
  await writeFile(resolve(output,'mainnet.raw.json'),raw);
  const assets = (await readdir(output)).filter(name =>
    ['mainnet.raw.json','solvyren-node-linux-x86_64.gz','solvyren-node-windows-x86_64.exe'].includes(name)).sort();
  const sums = await Promise.all(assets.map(async name => `${digest(await readFile(resolve(output,name)))}  ${basename(name)}`));
  await writeFile(resolve(output,'SHA256SUMS'),sums.join('\n')+'\n','ascii');
  console.log(output);
}
