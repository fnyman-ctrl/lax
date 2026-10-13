# Lax's site

The page at the project's GitHub Pages address (and later its own domain):
what Lax is and how to build it. Plain HTML and CSS with one small script for
the Copy button; no build step.

- `index.html`, `style.css`, `copy.js`, `theme.js`: the page. `theme.js` keeps the
  visitor's light or dark choice in their own browser (localStorage) only.
- The workflow `.github/workflows/pages.yml` copies in the fonts from
  `src/fonts` (OFL, licences included), the pixel salmon and the icon, and
  publishes on every change to `main`.
- The page loads nothing from anywhere else: no web fonts, no analytics, no
  trackers. Its Content-Security-Policy only allows its own files.

To look at it locally, put the same files together and serve the folder, for
example with `python3 -m http.server`.
