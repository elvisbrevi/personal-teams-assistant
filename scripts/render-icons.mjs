// Renders the app icon (desktop/icons/source/app.png: the lime face tile on a white background, kept
// outside the crate) to the images the portal, the landing and the desktop app use. iOS gets a full-bleed square cut just inside the tile, so its own
// rounded mask meets the tile's glossy rim; browsers and the PWA manifest get the tile with
// transparent corners. Usage: node scripts/render-icons.mjs   (Playwright from scripts/ui-preview/deps.mjs)
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { launch } from './ui-preview/deps.mjs';

const root = path.join(path.dirname(fileURLToPath(import.meta.url)), '..');
const source = `data:image/png;base64,${fs.readFileSync(path.join(root, 'desktop/icons/source/app.png')).toString('base64')}`;
// The tile in the source: its box, its corner radius and the lime of its rim. `edge` cuts its
// antialiased outline, which carries the white background, off the transparent corners.
const tile = { left: 121, top: 124, right: 1128, bottom: 1120, radius: 249, rim: '#a4db0a', edge: 3 };
const outputs = [
  { name: 'desktop/ui/apple-touch-icon.png', size: 180, inset: 16, opaque: true },
  { name: 'desktop/ui/icon-192.png', size: 192, inset: 0 },
  { name: 'desktop/ui/icon-512.png', size: 512, inset: 0 },
  { name: 'desktop/ui/favicon.png', size: 64, inset: 0 },
  // The landing's brand mark, tab icon and home-screen icon.
  { name: 'site/public/assets/icon.png', size: 256, inset: 0 },
  { name: 'site/public/assets/apple-touch-icon.png', size: 180, inset: 16, opaque: true },
  // The desktop app: window and tray icon (bundle.icon in tauri.conf.json), and Windows' icon.
  { name: 'desktop/icons/32x32.png', size: 32, inset: 0 },
  { name: 'desktop/icons/128x128.png', size: 128, inset: 0 },
  { name: 'desktop/icons/icon.ico', sizes: [16, 24, 32, 48, 64, 256], inset: 0 },
];
/** An ICO file whose entries are PNG images (Windows Vista and later read them). */
function ico(pngs) {
  const header = Buffer.alloc(6 + 16 * pngs.length);
  header.writeUInt16LE(0, 0); header.writeUInt16LE(1, 2); header.writeUInt16LE(pngs.length, 4);
  let offset = header.length;
  pngs.forEach(({ size, data }, index) => {
    const entry = 6 + 16 * index;
    header.writeUInt8(size >= 256 ? 0 : size, entry); header.writeUInt8(size >= 256 ? 0 : size, entry + 1);
    header.writeUInt16LE(1, entry + 4); header.writeUInt16LE(32, entry + 6);
    header.writeUInt32LE(data.length, entry + 8); header.writeUInt32LE(offset, entry + 12);
    offset += data.length;
  });
  return Buffer.concat([header, ...pngs.map(png => png.data)]);
}
const browser = await launch();
const page = await browser.newPage();
async function render(output) {
  const data = await page.evaluate(async ({ source, tile, output }) => {
    const image = new Image();
    image.src = source;
    await image.decode();
    // A square around the tile's center, `inset` source pixels inside its widest side.
    const side = tile.right - tile.left - 2 * output.inset;
    const x = (tile.left + tile.right - side) / 2, y = (tile.top + tile.bottom - side) / 2;
    const canvas = document.createElement('canvas');
    canvas.width = canvas.height = output.size;
    const g = canvas.getContext('2d');
    g.imageSmoothingQuality = 'high';
    const scale = output.size / side;
    g.scale(scale, scale);
    g.translate(-x, -y);
    if (output.opaque) { g.fillStyle = tile.rim; g.fillRect(x, y, side, side); }
    // Only the tile: the white background and its shadow become the rim's lime or transparent.
    g.beginPath();
    g.roundRect(tile.left + tile.edge, tile.top + tile.edge, tile.right - tile.left - 2 * tile.edge, tile.bottom - tile.top - 2 * tile.edge, tile.radius - tile.edge);
    g.clip();
    g.drawImage(image, 0, 0);
    return canvas.toDataURL('image/png');
  }, { source, tile, output });
  return Buffer.from(data.split(',')[1], 'base64');
}
for (const output of outputs) {
  const data = output.sizes
    ? ico(await Promise.all(output.sizes.map(async size => ({ size, data: await render({ ...output, size }) }))))
    : await render(output);
  fs.writeFileSync(path.join(root, output.name), data);
  console.log(`${output.name} (${output.sizes ? output.sizes.join(', ') : `${output.size} × ${output.size}`})`);
}
await browser.close();
