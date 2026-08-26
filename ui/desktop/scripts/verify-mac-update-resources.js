#!/usr/bin/env node

const fs = require('node:fs');
const path = require('node:path');

function fail(message) {
  console.error(message);
  process.exit(1);
}

const appPath = process.argv[2];
if (!appPath) {
  fail('Usage: node scripts/verify-mac-update-resources.js <path-to-app>');
}

const resourcesPath = path.join(appPath, 'Contents', 'Resources');
const requiredFiles = ['LICENSE', 'NOTICE', 'MODIFICATIONS.md', 'THIRD_PARTY_NOTICES.md'];

for (const fileName of requiredFiles) {
  const filePath = path.join(resourcesPath, fileName);
  if (!fs.existsSync(filePath)) {
    fail(`Missing ${filePath}`);
  }
}

console.log(`${resourcesPath} contains the required Lumina legal notices`);
