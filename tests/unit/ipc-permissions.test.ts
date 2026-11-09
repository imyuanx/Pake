import fs from 'node:fs';
import path from 'node:path';
import { describe, expect, it } from 'vitest';

const read = (file: string) =>
  fs.readFileSync(path.join(process.cwd(), 'src-tauri', file), 'utf8');

describe('native IPC permission registration', () => {
  it('puts every registered app command in the Tauri ACL manifest', () => {
    const handler = read('src/lib.rs').match(
      /tauri::generate_handler!\[([^\]]+)\]/,
    )?.[1];
    const manifest = read('build.rs').match(
      /tauri_build::AppManifest::new\(\)\.commands\(&\[([^\]]+)\]\)/,
    )?.[1];
    expect(handler).toBeTruthy();
    expect(manifest).toBeTruthy();
    const commands = handler!
      .split(',')
      .map((name) => name.trim())
      .filter(Boolean);
    const declared = [...manifest!.matchAll(/"([a-z_]+)"/g)].map(
      (match) => match[1],
    );
    for (const command of commands) expect(declared).toContain(command);
  });

  it('grants app commands, including autostart, to the packaged remote origin', () => {
    const capability = JSON.parse(read('capabilities/default.json'));
    expect(capability.remote?.urls).toContain(
      'https://fnos.tail5954c0.ts.net/*',
    );
    expect(capability.permissions).toEqual(
      expect.arrayContaining([
        'allow-download-file',
        'allow-reveal-download',
        'allow-open-window',
        'allow-open-external',
        'allow-get-autostart',
        'allow-set-autostart',
      ]),
    );
  });

  it('lets data-tauri-drag-region double-click toggle maximize on the remote origin', () => {
    const capability = JSON.parse(read('capabilities/default.json'));
    expect(capability.permissions).toEqual(
      expect.arrayContaining([
        'core:window:allow-start-dragging',
        'core:window:allow-internal-toggle-maximize',
      ]),
    );
  });
});
