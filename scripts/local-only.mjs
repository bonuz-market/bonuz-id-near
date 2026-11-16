// Preload for scripts/rehearse-mainnet-fork.ts: deploy.ts runs with its MAINNET profile
// against a local fork, so any request that is not to this machine fails loudly. Together
// with a credentials folder that holds only throwaway keys, nothing can reach mainnet.
const realFetch = globalThis.fetch;
globalThis.fetch = (input, init) => {
  const url = new URL(typeof input === 'string' || input instanceof URL ? String(input) : input.url);
  if (url.hostname !== '127.0.0.1' && url.hostname !== 'localhost') {
    process.stderr.write(`NETWORK CALL BLOCKED: ${url.origin}\n`);
    throw new Error(`only the local fork may be reached, not ${url.origin}`);
  }
  return realFetch(input, init);
};
