// Loads optional Starlight plugins via Vite SSR so packages shipping TypeScript sources
// (such as `@composable-delivery/muselab-ux-kit`) work cleanly, with an unbranded fallback
// when running on external contributor forks without private package access.
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { createServer } from 'vite';

/** @returns {Promise<{ plugins: any[], packages: string[], branded: boolean }>} */
export async function loadStarlightPlugins() {
  const explicit = (process.env.DOCS_STARLIGHT_PLUGINS ?? '')
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);

  // Branding is opt-in and explicit (set only by the Vercel deployment). A bare
  // NODE_AUTH_TOKEN must NOT enable it: CI runners set a placeholder token.
  const wantsMuselabBrand = process.env.MUSELAB_BRAND === 'true';

  const entries =
    explicit.length > 0
      ? explicit
      : wantsMuselabBrand
        ? ['@composable-delivery/muselab-ux-kit/starlight#muselabStarlightPlugin']
        : [];

  if (entries.length === 0) {
    return { plugins: [], packages: [], branded: false };
  }

  const bare = (s) => !s.startsWith('.') && !path.isAbsolute(s);
  const names = entries.map((e) => e.split('#')[0]).filter(bare);
  const vite = await createServer({
    configFile: false,
    logLevel: 'silent',
    appType: 'custom',
    server: { middlewareMode: true, hmr: false, watch: null },
    optimizeDeps: { noDiscovery: true, include: [] },
    ssr: { noExternal: names.map(packageName) },
  });

  const plugins = [];
  const packages = [];
  let branded = false;

  try {
    for (const entry of entries) {
      const hash = entry.indexOf('#');
      const specifier = hash < 0 ? entry : entry.slice(0, hash);
      const exportName = hash < 0 || hash === entry.length - 1 ? 'default' : entry.slice(hash + 1);
      const isPath = specifier.startsWith('.') || path.isAbsolute(specifier);

      try {
        const mod = await vite.ssrLoadModule(
          isPath ? pathToFileURL(path.resolve(specifier)).href : specifier
        );
        const exported = mod[exportName];
        if (exported === undefined) {
          throw new Error(`module has no export "${exportName}"`);
        }
        plugins.push(typeof exported === 'function' ? exported() : exported);
        if (!isPath) packages.push(packageName(specifier));
        if (specifier.includes('muselab-ux-kit')) branded = true;
      } catch (error) {
        if (explicit.length > 0 || wantsMuselabBrand) {
          // Asked for branding but it is unavailable: fail loudly rather than publish unbranded.
          throw new Error(
            `Starlight plugin: cannot load "${entry}": ${
              error instanceof Error ? error.message : error
            }`,
            { cause: error }
          );
        }
        console.warn(
          `[muselab-docs-ux] Could not load ${specifier}; falling back to unbranded Starlight.`
        );
      }
    }
  } finally {
    await vite.close();
  }

  return { plugins, packages, branded };
}

/** `@scope/pkg/sub/path` -> `@scope/pkg`. */
function packageName(specifier) {
  return specifier.split('/').slice(0, specifier.startsWith('@') ? 2 : 1).join('/');
}

