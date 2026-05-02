#!/usr/bin/env node

import { appendFile, readFile } from 'node:fs/promises';

const DEFAULT_CARGO_TOML = 'src-tauri/Cargo.toml';
const DEFAULT_CHANGELOG = 'CHANGELOG.md';

function parseArgs(argv) {
  const options = {
    cargoToml: DEFAULT_CARGO_TOML,
    changelog: DEFAULT_CHANGELOG,
    dryRun: false,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];

    if (arg === '--dry-run') {
      options.dryRun = true;
    } else if (arg === '--cargo-toml') {
      options.cargoToml = argv[++i];
    } else if (arg === '--changelog') {
      options.changelog = argv[++i];
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }

  return options;
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

function parseCargoVersion(content) {
  let inPackageSection = false;

  for (const line of content.split(/\r?\n/)) {
    const section = line.match(/^\s*\[([^\]]+)\]\s*$/)?.[1];

    if (section) {
      inPackageSection = section === 'package';
      continue;
    }

    if (inPackageSection) {
      const version = line.match(/^\s*version\s*=\s*"([^"]+)"\s*$/)?.[1];

      if (version) {
        return version;
      }
    }
  }

  throw new Error('Unable to read package.version from src-tauri/Cargo.toml');
}

function parseTopChangelogEntry(content) {
  let version;
  const bodyLines = [];

  for (const line of content.split(/\r?\n/)) {
    const heading = line.match(/^##\s+(.+)\s*$/)?.[1];

    if (heading) {
      if (version) {
        break;
      }

      version = heading.trim();
      continue;
    }

    if (version) {
      bodyLines.push(line);
    }
  }

  if (!version) {
    throw new Error('Unable to read the top CHANGELOG.md version entry');
  }

  const body = bodyLines.join('\n').trim();

  if (!body) {
    throw new Error(`CHANGELOG.md entry for ${version} is empty`);
  }

  return { version, body };
}

function githubHeaders(token) {
  return {
    Accept: 'application/vnd.github+json',
    Authorization: `Bearer ${token}`,
    'Content-Type': 'application/json',
    'User-Agent': 'm3u8quicker-release-creator',
    'X-GitHub-Api-Version': '2022-11-28',
  };
}

async function requestJson(url, options) {
  const response = await fetch(url, options);

  if (!response.ok) {
    const body = await response.text();
    throw new Error(`GitHub API request failed (${response.status}): ${body}`);
  }

  return response.json();
}

async function assertNotFound(url, options, existsMessage) {
  const response = await fetch(url, options);

  if (response.status === 404) {
    return;
  }

  if (response.ok) {
    throw new Error(existsMessage);
  }

  const body = await response.text();
  throw new Error(`GitHub API request failed (${response.status}): ${body}`);
}

async function writeOutput(name, value) {
  if (!process.env.GITHUB_OUTPUT) {
    return;
  }

  const delimiter = `__m3u8quicker_${name}_${Date.now()}__`;
  await appendFile(process.env.GITHUB_OUTPUT, `${name}<<${delimiter}\n${value}\n${delimiter}\n`);
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const [cargoToml, changelog] = await Promise.all([
    readFile(options.cargoToml, 'utf8'),
    readFile(options.changelog, 'utf8'),
  ]);
  const version = parseCargoVersion(cargoToml);
  const changelogEntry = parseTopChangelogEntry(changelog);

  if (changelogEntry.version !== version) {
    throw new Error(
      `Cargo version (${version}) does not match top CHANGELOG.md version (${changelogEntry.version})`,
    );
  }

  const tagName = version;

  if (options.dryRun) {
    console.log(`Dry run: would create release ${tagName}`);
    console.log(changelogEntry.body);
    await writeOutput('release_id', '');
    await writeOutput('tag_name', tagName);
    await writeOutput('prerelease', 'false');
    return;
  }

  const repo = process.env.GITHUB_REPOSITORY;
  const token = process.env.GITHUB_TOKEN;
  const targetCommitish = process.env.GITHUB_SHA;

  if (!repo) {
    throw new Error('GITHUB_REPOSITORY is required');
  }

  if (!token) {
    throw new Error('GITHUB_TOKEN is required');
  }

  if (!targetCommitish) {
    throw new Error('GITHUB_SHA is required');
  }

  const baseUrl = `https://api.github.com/repos/${repo}`;
  const headers = githubHeaders(token);

  await assertNotFound(
    `${baseUrl}/releases/tags/${encodeURIComponent(tagName)}`,
    { headers },
    `Release already exists for tag: ${tagName}`,
  );
  await assertNotFound(
    `${baseUrl}/git/ref/tags/${encodeURIComponent(tagName)}`,
    { headers },
    `Git tag already exists: ${tagName}`,
  );

  const release = await requestJson(`${baseUrl}/releases`, {
    method: 'POST',
    headers,
    body: JSON.stringify({
      tag_name: tagName,
      target_commitish: targetCommitish,
      name: tagName,
      body: changelogEntry.body,
      draft: false,
      prerelease: false,
    }),
  });

  console.log(`Created release ${tagName} (${release.id})`);
  await writeOutput('release_id', String(release.id));
  await writeOutput('tag_name', tagName);
  await writeOutput('prerelease', 'false');
}

main().catch((error) => fail(error.message));
