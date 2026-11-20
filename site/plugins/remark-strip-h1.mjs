// Starlight already renders `frontmatter.title` as the page `<h1>`. Removing a leading
// top-level `# Heading` in the Markdown body keeps `docs/*.md` readable on GitHub without
// rendering duplicate `<h1>` headings on the Starlight site.
export default function remarkStripLeadingH1() {
  return (tree) => {
    const first = tree.children?.[0];
    if (first && first.type === 'heading' && first.depth === 1) {
      tree.children.shift();
    }
  };
}

