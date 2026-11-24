# bacnet-kit website

The landing page and field guide build as a static site with Zola 0.23.6. From this directory, run:

```sh
zola serve
```

Open the address Zola prints. Edit guides in `content/docs/`, layouts in `templates/`, and styles and interactive examples in `static/`. Each guide’s front matter provides its title, search description, navigation group and filter terms.

## Check and publish

```sh
zola check
zola build
```

Zola writes HTML, assets, a sitemap and robots.txt to `public/`. The production base URL in `config.toml` includes the repository prefix. Use Zola content links in Markdown and `get_url` in templates so links work in both preview and production.

The GitHub Actions workflow builds pull requests and deploys changes from `main` to GitHub Pages. GitHub Pages uses the **GitHub Actions** publishing source. [Site workflow](../.github/workflows/site.yml)
