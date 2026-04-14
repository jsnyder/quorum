// Test fixture: sync-in-async rule should match synchronous fs calls inside async functions
import * as fs from 'fs';

async function loadConfig() {
  const data = fs.readFileSync('/etc/config.json');
  return JSON.parse(data.toString());
}
