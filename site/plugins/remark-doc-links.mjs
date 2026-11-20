// Rewrites relative Markdown links in `docs/`, `PORTING.md`, and `BENCHMARKS.md` so links work
// both when browsing the repository on GitHub and in the published Astro Starlight site.
import path from 'node:path';

const HAS_SCHEME = /^[a-z][a-z0-9+.-]*:/i;

/**
 * @param {object} options
 * @param {string} options.repoRoot
 * @param {string} options.docsRoot
 * @param {string} options.generatedRoot
 * @param {string} options.base
 * @param {string} options.repoBlobUrl
 * @param {string} [options.canonicalUrl]
 * @param {Record<string, string>} [options.generatedOrigins]
 */
export default function remarkDocLinks({
  repoRoot,
  docsRoot,
  generatedRoot,
  base,
  repoBlobUrl,
  canonicalUrl,
  generatedOrigins = {},
}) {
  const prefix = `/${base}/`.replace(/\/+/g, '/');
  const canonical = canonicalUrl ? canonicalUrl.replace(/\/*$/, '/') : null;
  const blob = repoBlobUrl.replace(/\/*$/, '/');

  function sourcePathFor(filePath) {
    const relGen = path.relative(generatedRoot, filePath).split(path.sep).join('/');
    if (!relGen.startsWith('..') && generatedOrigins[relGen]) {
      return path.join(repoRoot, generatedOrigins[relGen]);
    }
    return filePath;
  }

  function rewrite(url, originFilePath) {
    if (canonical && url.startsWith(canonical)) return prefix + url.slice(canonical.length);
    if (!url || url.startsWith('#') || url.startsWith('/') || HAS_SCHEME.test(url)) return url;

    const [target, hash = ''] = url.split(/(?=#)/);
    if (!target) return url;

    const resolved = path.resolve(path.dirname(originFilePath), target);
    const repoRel = path.relative(repoRoot, resolved).split(path.sep).join('/');
    if (repoRel.startsWith('..')) return url;

    if (repoRel === 'BENCHMARKS.md') return `${prefix}benchmarks/${hash}`;
    if (repoRel === 'PORTING.md') return `${prefix}porting/${hash}`;

    const docRel = path.relative(docsRoot, resolved).split(path.sep).join('/');
    if (!docRel.startsWith('..') && docRel.endsWith('.md')) {
      const slug = docRel.slice(0, -3).replace(/(^|\/)index$/, '');
      return `${prefix}${slug ? `${slug}/` : ''}${hash}`;
    }

    return `${blob}${repoRel}${hash}`;
  }

  return (tree, file) => {
    const filePath = file.path ?? file.history?.[0];
    if (!filePath) return;
    const originFilePath = sourcePathFor(filePath);
    const walk = (node) => {
      if ((node.type === 'link' || node.type === 'definition') && typeof node.url === 'string') {
        node.url = rewrite(node.url, originFilePath);
      }
      node.children?.forEach(walk);
    };
    walk(tree);
  };
}

