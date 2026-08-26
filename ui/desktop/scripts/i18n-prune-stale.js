#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const messagesDirectory = path.join(__dirname, '..', 'src', 'i18n', 'messages');
const sourcePath = path.join(messagesDirectory, 'en.json');
const sourceMessages = JSON.parse(fs.readFileSync(sourcePath, 'utf8'));
const activeKeys = new Set(Object.keys(sourceMessages));

for (const fileName of fs.readdirSync(messagesDirectory).sort()) {
  if (!fileName.endsWith('.json') || fileName === 'en.json') continue;
  const filePath = path.join(messagesDirectory, fileName);
  const messages = JSON.parse(fs.readFileSync(filePath, 'utf8'));
  const activeMessages = Object.fromEntries(
    Object.entries(messages).filter(([key]) => activeKeys.has(key))
  );
  fs.writeFileSync(filePath, `${JSON.stringify(activeMessages, null, 2)}\n`, 'utf8');
}
