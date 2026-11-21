// Single source of truth for where these docs are published and which repo they document.
// The published URL is always <origin>/<repo-name>/ (the shared docs host serves one path per repo).
// Everything that needs the URL imports it from here; scripts/check-docs-url.mjs verifies the places
// that cannot import it (Markdown, install scripts, vercel.json) agree with it.
export const DOCS_ORIGIN = 'https://docs.muselab.com';
export const GITHUB_ORG = 'composable-delivery';
export const REPO_NAME = 'busbar-sf-source-format';

export const DOCS_BASE = `/${REPO_NAME}`;
export const DOCS_URL = `${DOCS_ORIGIN}${DOCS_BASE}/`;
export const REPO_URL = `https://github.com/${GITHUB_ORG}/${REPO_NAME}`;
