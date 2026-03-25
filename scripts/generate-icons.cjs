// 生成 AIChat Updater 图标
const fs = require('fs');
const path = require('path');

// 创建一个简单的 PNG 图标 (512x512)
// 使用纯 Node.js buffer 创建 PNG

function createPNG(width, height) {
  // PNG 文件头
  const signature = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
  
  // IHDR chunk
  const ihdrData = Buffer.alloc(13);
  ihdrData.writeUInt32BE(width, 0);   // width
  ihdrData.writeUInt32BE(height, 4);  // height
  ihdrData.writeUInt8(8, 8);          // bit depth
  ihdrData.writeUInt8(2, 9);          // color type (RGB)
  ihdrData.writeUInt8(0, 10);         // compression
  ihdrData.writeUInt8(0, 11);         // filter
  ihdrData.writeUInt8(0, 12);         // interlace
  
  const ihdr = createChunk('IHDR', ihdrData);
  
  // 创建图像数据 - 渐变背景 + 简单的 "AI" 文字图案
  const rawData = Buffer.alloc((width * 3 + 1) * height);
  
  for (let y = 0; y < height; y++) {
    const rowStart = y * (width * 3 + 1);
    rawData[rowStart] = 0; // filter byte (none)
    
    for (let x = 0; x < width; x++) {
      const px = rowStart + 1 + x * 3;
      const cx = x / width;
      const cy = y / height;
      
      // 圆角矩形检测
      const margin = width * 0.08;
      const radius = width * 0.15;
      const inRoundRect = isInRoundRect(x, y, margin, margin, width - margin * 2, height - margin * 2, radius);
      
      if (inRoundRect) {
        // 渐变背景 (从蓝紫到紫色)
        const r = Math.floor(102 + (118 - 102) * cy);
        const g = Math.floor(126 + (75 - 126) * cy);
        const b = Math.floor(234 + (162 - 234) * cy);
        
        // 检测是否在 "AI" 字母区域
        const inLetter = isInAI(x, y, width, height);
        
        if (inLetter) {
          rawData[px] = 255;     // R - 白色文字
          rawData[px + 1] = 255; // G
          rawData[px + 2] = 255; // B
        } else {
          rawData[px] = r;
          rawData[px + 1] = g;
          rawData[px + 2] = b;
        }
      } else {
        // 透明区域用白色
        rawData[px] = 255;
        rawData[px + 1] = 255;
        rawData[px + 2] = 255;
      }
    }
  }
  
  // 压缩数据
  const zlib = require('zlib');
  const compressed = zlib.deflateSync(rawData);
  const idat = createChunk('IDAT', compressed);
  
  // IEND chunk
  const iend = createChunk('IEND', Buffer.alloc(0));
  
  return Buffer.concat([signature, ihdr, idat, iend]);
}

function isInRoundRect(x, y, rx, ry, rw, rh, radius) {
  if (x < rx || x > rx + rw || y < ry || y > ry + rh) return false;
  
  // 检查四个角
  const corners = [
    [rx + radius, ry + radius],
    [rx + rw - radius, ry + radius],
    [rx + radius, ry + rh - radius],
    [rx + rw - radius, ry + rh - radius]
  ];
  
  for (const [cx, cy] of corners) {
    const dx = Math.abs(x - cx);
    const dy = Math.abs(y - cy);
    if (dx > radius || dy > radius) continue;
    if (x >= cx - radius && x <= cx + radius && y >= cy - radius && y <= cy + radius) {
      if (dx * dx + dy * dy > radius * radius) {
        // 只有在角落区域才检查圆角
        if ((x < rx + radius || x > rx + rw - radius) && (y < ry + radius || y > ry + rh - radius)) {
          return false;
        }
      }
    }
  }
  
  return true;
}

function isInAI(x, y, w, h) {
  const cx = x / w;
  const cy = y / h;
  
  // "A" 字母 区域 (左侧)
  // A 的左斜线
  const aLeft = 0.2;
  const aRight = 0.48;
  const aTop = 0.25;
  const aBottom = 0.75;
  const aCenterX = (aLeft + aRight) / 2;
  const strokeW = 0.04;
  
  // A 的左斜边
  if (cy >= aTop && cy <= aBottom) {
    const progress = (cy - aTop) / (aBottom - aTop);
    const leftEdge = aCenterX - progress * (aRight - aLeft) / 2;
    if (cx >= leftEdge - strokeW / 2 && cx <= leftEdge + strokeW / 2) return true;
    
    // A 的右斜边
    const rightEdge = aCenterX + progress * (aRight - aLeft) / 2;
    if (cx >= rightEdge - strokeW / 2 && cx <= rightEdge + strokeW / 2) return true;
  }
  
  // A 的横杠
  if (cy >= 0.55 && cy <= 0.55 + strokeW) {
    const progress = (0.55 - aTop) / (aBottom - aTop);
    const leftEdge = aCenterX - progress * (aRight - aLeft) / 2;
    const rightEdge = aCenterX + progress * (aRight - aLeft) / 2;
    if (cx >= leftEdge && cx <= rightEdge) return true;
  }
  
  // "I" 字母 (右侧)
  const iCenterX = 0.65;
  if (cx >= iCenterX - strokeW / 2 && cx <= iCenterX + strokeW / 2 && cy >= aTop && cy <= aBottom) {
    return true;
  }
  // I 的上横线
  if (cy >= aTop && cy <= aTop + strokeW && cx >= 0.55 && cx <= 0.75) {
    return true;
  }
  // I 的下横线
  if (cy >= aBottom - strokeW && cy <= aBottom && cx >= 0.55 && cx <= 0.75) {
    return true;
  }
  
  return false;
}

function createChunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  
  const typeBuffer = Buffer.from(type, 'ascii');
  const crcData = Buffer.concat([typeBuffer, data]);
  
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(crcData), 0);
  
  return Buffer.concat([length, typeBuffer, data, crc]);
}

function crc32(data) {
  let crc = 0xffffffff;
  for (let i = 0; i < data.length; i++) {
    crc ^= data[i];
    for (let j = 0; j < 8; j++) {
      if (crc & 1) {
        crc = (crc >>> 1) ^ 0xedb88320;
      } else {
        crc = crc >>> 1;
      }
    }
  }
  return (crc ^ 0xffffffff) >>> 0;
}

// 生成各种尺寸
const iconsDir = path.join(__dirname, '..', 'src-tauri', 'icons');

const sizes = [32, 128, 256, 512];

for (const size of sizes) {
  const png = createPNG(size, size);
  const filename = size === 256 ? '128x128@2x.png' : `${size}x${size}.png`;
  fs.writeFileSync(path.join(iconsDir, filename), png);
  console.log(`Generated ${filename}`);
}

// 512x512 用作 icon.png
const png512 = createPNG(512, 512);
fs.writeFileSync(path.join(iconsDir, 'icon.png'), png512);
console.log('Generated icon.png');

console.log('All icons generated!');
