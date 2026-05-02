#!/usr/bin/env node

import { readFile, writeFile } from 'node:fs/promises';

const DEFAULT_README = 'README.md';
const DIRECT_DOWNLOAD_RE =
  /https:\/\/github\.com\/([^/\s)]+\/[^/\s)]+)\/releases\/latest\/download\/([^\s)]+)/g;

const REQUIRED_ASSETS = [
  {
    key: 'windows-x64-setup',
    pattern: /^M3U8\.Quicker_.+_windows_x64_setup\.exe$/,
  },
  {
    key: 'windows-x64-zip',
    pattern: /^M3U8\.Quicker_.+_windows_x64\.zip$/,
  },
  {
    key: 'windows-x86-setup',
    pattern: /^M3U8\.Quicker_.+_windows_x86_setup\.exe$/,
  },
  {
    key: 'windows-x86-zip',
    pattern: /^M3U8\.Quicker_.+_windows_x86\.zip$/,
  },
  {
    key: 'macos-aarch64-dmg',
    pattern: /^M3U8\.Quicker_.+_macos_aarch64\.dmg$/,
  },
  {
    key: 'macos-aarch64-app',
    pattern: /^M3U8\.Quicker_.+_macos_aarch64\.app\.tar\.gz$/,
  },
  {
    key: 'macos-x64-dmg',
    pattern: /^M3U8\.Quicker_.+_macos_x64\.dmg$/,
  },
  {
    key: 'macos-x64-app',
    pattern: /^M3U8\.Quicker_.+_macos_x64\.app\.tar\.gz$/,
  },
  {
    key: 'linux-appimage',
    pattern: /^M3U8\.Quicker_.+_linux_amd64\.AppImage$/,
  },
  {
    key: 'linux-deb',
    pattern: /^M3U8\.Quicker_.+_linux_amd64\.deb$/,
  },
  {
    key: 'linux-rpm',
    pattern: /^M3U8\.Quicker_.+_linux_x86_64\.rpm$/,
  },
];

function parseArgs(argv) {
  const options = {
    check: false,
    dryRun: false,
    readme: DEFAULT_README,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];

    if (arg === '--check') {
      options.check = true;
    } else if (arg === '--dry-run') {
      options.dryRun = true;
    } else if (arg === '--assets-file') {
      options.assetsFile = argv[++i];
    } else if (arg === '--readme') {
      options.readme = argv[++i];
    } else if (arg === '--release-id') {
      options.releaseId = argv[++i];
    } else if (arg === '--repo') {
      options.repo = argv[++i];
    } else if (arg === '--tag') {
      options.tag = argv[++i];
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }

  options.repo ||= process.env.GITHUB_REPOSITORY || 'Liubsyy/M3U8Quicker';
  options.releaseId ||= process.env.RELEASE_ID || process.env.GITHUB_RELEASE_ID;
  options.tag ||= process.env.RELEASE_TAG || process.env.GITHUB_REF_NAME;

  return options;
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function extractAssetNames(value) {
  if (Array.isArray(value)) {
    return value.map((asset) => (typeof asset === 'string' ? asset : asset.name)).filter(Boolean);
  }

  if (Array.isArray(value?.assets)) {
    return value.assets.map((asset) => asset.name).filter(Boolean);
  }

  return [];
}

async function readAssetNamesFromFile(path) {
  const raw = await readFile(path, 'utf8');

  try {
    const parsed = JSON.parse(raw);
    const names = extractAssetNames(parsed);
    if (names.length > 0) {
      return names;
    }
  } catch {
    // Fall back to one asset name per line below.
  }

  return raw
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean);
}

async function fetchReleaseAssetNames({ releaseId, repo, tag }) {
  const releasePath = releaseId
    ? `releases/${releaseId}`
    : tag
      ? `releases/tags/${encodeURIComponent(tag)}`
      : 'releases/latest';
  const url = `https://api.github.com/repos/${repo}/${releasePath}`;
  const headers = {
    Accept: 'application/vnd.github+json',
    'User-Agent': 'm3u8quicker-release-link-updater',
    'X-GitHub-Api-Version': '2022-11-28',
  };
  const token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;

  if (token) {
    headers.Authorization = `Bearer ${token}`;
  }

  const response = await fetch(url, { headers });
  if (!response.ok) {
    const body = await response.text();
    throw new Error(`Failed to fetch release assets (${response.status}): ${body}`);
  }

  const release = await response.json();
  const names = extractAssetNames(release);
  if (names.length === 0) {
    throw new Error(`Release has no assets: ${url}`);
  }

  return names;
}

function resolveRequiredAssets(assetNames) {
  const resolved = new Map();

  for (const required of REQUIRED_ASSETS) {
    const matches = assetNames.filter((name) => required.pattern.test(name));

    if (matches.length === 0) {
      throw new Error(`Missing required release asset: ${required.key}`);
    }

    if (matches.length > 1) {
      throw new Error(`Multiple release assets match ${required.key}: ${matches.join(', ')}`);
    }

    resolved.set(required.key, matches[0]);
  }

  return resolved;
}

function findRequiredAssetKey(fileName) {
  const decodedName = decodeURIComponent(fileName);
  return REQUIRED_ASSETS.find((asset) => asset.pattern.test(decodedName))?.key;
}

function createDownloadUrl(repo, assetName) {
  return `https://github.com/${repo}/releases/latest/download/${encodeURIComponent(assetName)}`;
}

function updateReadmeLinks(content, repo, assetsByKey) {
  const seen = new Set();
  const updated = content.replace(DIRECT_DOWNLOAD_RE, (url, matchedRepo, fileName) => {
    if (matchedRepo.toLowerCase() !== repo.toLowerCase()) {
      return url;
    }

    const key = findRequiredAssetKey(fileName);
    if (!key) {
      return url;
    }

    seen.add(key);
    return createDownloadUrl(repo, assetsByKey.get(key));
  });

  const missingLinks = REQUIRED_ASSETS.filter((asset) => !seen.has(asset.key)).map((asset) => asset.key);
  if (missingLinks.length > 0) {
    throw new Error(`README is missing direct download links for: ${missingLinks.join(', ')}`);
  }

  return updated;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const assetNames = options.assetsFile
    ? await readAssetNamesFromFile(options.assetsFile)
    : await fetchReleaseAssetNames(options);
  const assetsByKey = resolveRequiredAssets(assetNames);
  const readme = await readFile(options.readme, 'utf8');
  const updatedReadme = updateReadmeLinks(readme, options.repo, assetsByKey);

  if (updatedReadme === readme) {
    console.log('README download links are already up to date.');
    return;
  }

  if (options.check) {
    fail('README download links are not up to date.');
  }

  if (options.dryRun) {
    console.log('Dry run: README download links would be updated.');
    return;
  }

  await writeFile(options.readme, updatedReadme);
  console.log('Updated README download links.');
}

main().catch((error) => fail(error.message));
