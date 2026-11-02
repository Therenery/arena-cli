const fs = require('fs'),
  zlib = require('zlib');
const p = process.argv[2];
const b = zlib.gzipSync(fs.readFileSync(p)).toString('base64');
process.stdout.write('##B64##' + b + '##END##\n');
