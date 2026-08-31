// Generates a 1024x1024 app-icon source PNG for `tauri icon`.
// Dark navy backdrop + blue rounded square + white play triangle.
import { deflateSync } from 'node:zlib'
import { writeFileSync } from 'node:fs'
import { resolve } from 'node:path'

const SIZE = 1024
const px = new Uint8Array(SIZE * SIZE * 4)

function set(x, y, r, g, b, a = 255) {
  if (x < 0 || y < 0 || x >= SIZE || y >= SIZE) return
  const i = (y * SIZE + x) * 4
  px[i] = r; px[i + 1] = g; px[i + 2] = b; px[i + 3] = a
}

// Inside a rounded rect? cx,cy=center; half=half-size; rad=corner radius.
function inRounded(cx, cy, half, rad, x, y) {
  const dx = Math.max(Math.abs(x - cx) - (half - rad), 0)
  const dy = Math.max(Math.abs(y - cy) - (half - rad), 0)
  return dx * dx + dy * dy <= rad * rad
}

// Inside a right-pointing play triangle?
function inPlayTriangle(x, y) {
  // vertices: top(0,-1) bottom(0,1) right(1,0), scaled+offset
  const [ax, ay] = [-0.5, -0.62]
  const [bx, by] = [-0.5, 0.62]
  const [cx, cy] = [0.78, 0.0]
  const sign = (p1x, p1y, p2x, p2y, p3x, p3y) =>
    (p1x - p3x) * (p2y - p3y) - (p2x - p3x) * (p1y - p3y)
  const d1 = sign(x, y, ax, ay, bx, by)
  const d2 = sign(x, y, bx, by, cx, cy)
  const d3 = sign(x, y, cx, cy, ax, ay)
  const hasNeg = d1 < 0 || d2 < 0 || d3 < 0
  const hasPos = d1 > 0 || d2 > 0 || d3 > 0
  return !(hasNeg && hasPos)
}

for (let y = 0; y < SIZE; y++) {
  for (let x = 0; x < SIZE; x++) {
    set(x, y, 11, 18, 32) // #0B1220 bg

    // blue rounded square
    if (inRounded(SIZE / 2, SIZE / 2, 250, 84, x, y)) {
      const t = y / SIZE
      const r = Math.round(37 + (59 - 37) * t)
      const g = Math.round(99 + (130 - 99) * t)
      const b = Math.round(235 + (243 - 235) * t)
      set(x, y, r, g, b)
    }

    // white play triangle
    if (inPlayTriangle((x - SIZE / 2) / (SIZE / 2), (y - SIZE / 2) / (SIZE / 2))) {
      set(x, y, 255, 255, 255)
    }
  }
}

// --- PNG encoding (RGBA, filter 0, deflate) ---
function crc32(buf) {
  let c = 0xffffffff
  for (let i = 0; i < buf.length; i++) {
    c ^= buf[i]
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1
  }
  return (c ^ 0xffffffff) >>> 0
}

function chunk(type, data) {
  const len = Buffer.alloc(4)
  len.writeUInt32BE(data.length)
  const typeBuf = Buffer.from(type, 'ascii')
  const crcBuf = Buffer.alloc(4)
  crcBuf.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])))
  return Buffer.concat([len, typeBuf, data, crcBuf])
}

const ihdr = Buffer.alloc(13)
ihdr.writeUInt32BE(SIZE, 0)
ihdr.writeUInt32BE(SIZE, 4)
ihdr[8] = 8   // bit depth
ihdr[9] = 6   // color type RGBA
// 10..12: compression/filter/interlace = 0

const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1))
for (let y = 0; y < SIZE; y++) {
  raw[y * (SIZE * 4 + 1)] = 0 // filter: none
  px.subarray(y * SIZE * 4, (y + 1) * SIZE * 4).forEach((v, i) => {
    raw[y * (SIZE * 4 + 1) + 1 + i] = v
  })
}

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk('IHDR', ihdr),
  chunk('IDAT', deflateSync(raw, { level: 9 })),
  chunk('IEND', Buffer.alloc(0)),
])

const out = resolve(process.cwd(), 'apps/desktop/icon-source.png')
writeFileSync(out, png)
console.log(`wrote ${out} (${png.length} bytes)`)
