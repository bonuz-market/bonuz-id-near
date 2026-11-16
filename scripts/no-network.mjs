// Preload for scripts/rehearse-deploy.ts: any network call fails loudly. Used to prove
// that deploy.ts refuses dangerous flags before it reads anything from a chain.
const deny = (what) => {
  process.stderr.write(`NETWORK CALL: ${what}\n`);
  throw new Error(`network call blocked: ${what}`);
};
globalThis.fetch = (url) => deny(String(url));
