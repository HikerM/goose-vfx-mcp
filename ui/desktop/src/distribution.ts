import { app } from 'electron';
import * as fs from 'fs';
import * as path from 'path';
import {
  DEFAULT_DISTRIBUTION,
  DistributionInfo,
  parseDistributionInfo,
} from './distribution-config';

export function getDistributionInfo(): DistributionInfo {
  if (process.env.LUMINA_DISTRIBUTION_MODE === 'github') {
    return parseDistributionInfo({ mode: 'github' });
  }

  const configPath = app.isPackaged
    ? path.join(process.resourcesPath, 'custom-distribution.json')
    : path.join(app.getAppPath(), 'src', 'custom-distribution.json');

  try {
    return parseDistributionInfo(JSON.parse(fs.readFileSync(configPath, 'utf8')));
  } catch {
    return DEFAULT_DISTRIBUTION;
  }
}
