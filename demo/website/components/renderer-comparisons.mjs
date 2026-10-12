// Copyright © The Daybrite Project
// SPDX-License-Identifier: MPL-2.0
// Pair only captures from the same channel, target, and variant. Publication labels come
// from the dayscript's localized metadata, carried by the generated gallery manifest.
export function rendererComparisons(gallery, locale) {
  const localized = (text) => typeof text === 'string' ? text
    : text?.[locale] ?? text?.[locale.split('-')[0]] ?? text?.en ?? Object.values(text ?? {})[0];
  const pairs = [];
  for (const scene of ['bundled', 'resources']) {
    const native = gallery?.shots.find((shot) => shot.id === `renderer-${scene}-webview`);
    const servo = gallery?.shots.find((shot) => shot.id === `renderer-${scene}-servo`);
    if (!native || !servo) continue;
    for (const [target, variants] of Object.entries(native.byPlatform)) {
      for (const [variant, capture] of Object.entries(variants)) {
        const other = servo.byPlatform[target]?.[variant];
        if (!other) continue;
        const title = localized(native.caption);
        const nativeTitle = localized(native.title);
        const servoTitle = localized(servo.title);
        if (!title || !nativeTitle || !servoTitle) continue;
        pairs.push({
          id: `${target}-${scene}-${variant}`, title: `${title} · ${target}`,
          captures: [
            { ...capture, title: nativeTitle, shot: native.id },
            { ...other, title: servoTitle, shot: servo.id },
          ],
        });
      }
    }
  }
  return pairs;
}
