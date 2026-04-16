#!/usr/bin/env node

import { execFileSync } from 'node:child_process';
import { mkdir, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, '..');
const appUrl = process.env.APP_URL ?? 'http://127.0.0.1:4100';
const outputDir = path.join(rootDir, 'sprites', 'llm-fallback');

const fallbackSets = [
  {
    slug: '01-violet-crystal',
    description:
      'A small violet crystal dragon with glowing amber eyes, faceted wings, and a friendly but mischievous personality.',
  },
  {
    slug: '02-moss-forest',
    description:
      'A tiny moss-green forest dragon with leaf-shaped ears, warm golden eyes, and a shy woodland charm.',
  },
  {
    slug: '03-sunset-coral',
    description:
      'A bright coral-orange sunset dragon with cream belly scales, teal eyes, and an energetic playful spirit.',
  },
  {
    slug: '04-midnight-moon',
    description:
      'A small midnight-blue moon dragon with silver freckles, pale eyes, and a gentle dreamy aura.',
  },
];

const expectedEmotions = ['neutral', 'happy', 'angry', 'sleepy'];

function gitHead() {
  try {
    return execFileSync('git', ['rev-parse', 'HEAD'], {
      cwd: rootDir,
      encoding: 'utf8',
    }).trim();
  } catch {
    return 'unknown';
  }
}

async function getJson(url, init = {}) {
  const response = await fetch(url, init);
  const bodyText = await response.text();
  let json;

  try {
    json = JSON.parse(bodyText);
  } catch (error) {
    throw new Error(`Non-JSON response from ${url}: ${bodyText.slice(0, 500)}`);
  }

  if (!response.ok) {
    throw new Error(`HTTP ${response.status} from ${url}: ${JSON.stringify(json)}`);
  }

  return json;
}

async function postJson(endpoint, payload) {
  return getJson(`${appUrl}${endpoint}`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      origin: appUrl,
    },
    body: JSON.stringify(payload),
  });
}

async function ensureLiveApp() {
  const result = await getJson(`${appUrl}/api/live`);
  if (!result.ok) {
    throw new Error(`Live endpoint did not return ok=true: ${JSON.stringify(result)}`);
  }
}

function validateSpritePayload(result, slug) {
  if (!result.ok || !result.sprites) {
    throw new Error(`Sprite generation failed for ${slug}: ${JSON.stringify(result)}`);
  }

  const keys = Object.keys(result.sprites).sort();
  const expected = [...expectedEmotions].sort();
  if (JSON.stringify(keys) !== JSON.stringify(expected)) {
    throw new Error(
      `Unexpected sprite keys for ${slug}: got ${keys.join(', ')}, expected ${expected.join(', ')}`,
    );
  }
}

async function generateSpritesWithRetry(sessionCode, reconnectToken, spec) {
  let lastError;

  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      const result = await postJson('/api/workshops/sprite-sheet', {
        sessionCode,
        reconnectToken,
        description: spec.description,
      });

      validateSpritePayload(result, spec.slug);
      return result.sprites;
    } catch (error) {
      lastError = error;
      if (attempt < 3) {
        await new Promise((resolve) => setTimeout(resolve, 1000));
      }
    }
  }

  throw lastError;
}

async function main() {
  await ensureLiveApp();

  const createResult = await postJson('/api/workshops', {
    name: 'Fallback Sprite Builder',
    config: {
      phase0Minutes: 5,
      phase1Minutes: 10,
      phase2Minutes: 10,
    },
  });

  const sessionCode = createResult.sessionCode;
  const reconnectToken = createResult.reconnectToken;

  if (!sessionCode || !reconnectToken) {
    throw new Error(`Workshop creation returned unexpected payload: ${JSON.stringify(createResult)}`);
  }

  await postJson('/api/workshops/command', {
    sessionCode,
    reconnectToken,
    command: 'startPhase0',
  });

  await rm(outputDir, { recursive: true, force: true });
  await mkdir(outputDir, { recursive: true });

  const generatedAt = new Date().toISOString();
  const commit = gitHead();
  const manifest = {
    generatedAt,
    gitCommit: commit,
    appUrl,
    promptSource: {
      route: '/api/workshops/sprite-sheet',
      systemPromptFunction: 'build_sprite_sheet_system_instruction',
      userPromptFunction: 'build_sprite_sheet_user_prompt',
      sourceFile: 'platform/app-server/src/llm.rs',
      slicingContract: '2x2 grid -> neutral, happy, angry, sleepy with 5% quadrant crop',
    },
    sets: [],
  };

  for (const spec of fallbackSets) {
    const dir = path.join(outputDir, spec.slug);
    await mkdir(dir, { recursive: true });

    const sprites = await generateSpritesWithRetry(sessionCode, reconnectToken, spec);

    for (const emotion of expectedEmotions) {
      const png = Buffer.from(sprites[emotion], 'base64');
      await writeFile(path.join(dir, `${emotion}.png`), png);
    }

    const meta = {
      slug: spec.slug,
      description: spec.description,
      generatedAt,
      emotions: expectedEmotions,
    };
    await writeFile(path.join(dir, 'meta.json'), `${JSON.stringify(meta, null, 2)}\n`);

    manifest.sets.push({
      slug: spec.slug,
      description: spec.description,
      directory: path.relative(rootDir, dir),
      emotions: expectedEmotions,
    });
  }

  const readme = `# LLM Fallback Sprites

These fallback sprites were generated through the live \`POST /api/workshops/sprite-sheet\` endpoint.
That endpoint uses the current sprite prompt in \`platform/app-server/src/llm.rs\`, not a copied prompt.

Current slicing contract:
- top-left: neutral
- top-right: happy
- bottom-left: angry
- bottom-right: sleepy

Each set directory contains:
- \`neutral.png\`
- \`happy.png\`
- \`angry.png\`
- \`sleepy.png\`
- \`meta.json\`
`;

  await writeFile(path.join(outputDir, 'README.md'), readme);
  await writeFile(path.join(outputDir, 'manifest.json'), `${JSON.stringify(manifest, null, 2)}\n`);

  console.log(`Generated ${manifest.sets.length} fallback sprite sets in ${path.relative(rootDir, outputDir)}`);
  for (const entry of manifest.sets) {
    console.log(`- ${entry.slug}`);
  }
}

await main();
