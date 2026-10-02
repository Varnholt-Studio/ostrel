import { randomBytes } from 'node:crypto';
import { privateKeyFromSeed } from './paseto.ts';
import { createApp } from './app.ts';
import { PrismaStore } from './store.ts';

// REF_A_SERVER_SEED: 32 byte Ed25519 seed as hex, so a harness can mint tokens itself.
const seed = process.env.REF_A_SERVER_SEED ? Buffer.from(process.env.REF_A_SERVER_SEED, 'hex') : randomBytes(32);
const port = Number(process.env.PORT ?? 3000);

createApp(new PrismaStore(), privateKeyFromSeed(seed)).listen(port, () => {
  console.log(`REF-A server listening on port ${port}`);
});
