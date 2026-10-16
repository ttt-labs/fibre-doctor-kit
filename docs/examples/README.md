# Offline report example

[Open the HTML file](report.html), download it from GitHub, and open the downloaded file in a browser. The Doctor Fibre Kit report works offline and uses English. [report.json](report.json) contains the same public comparison data; it is a report, not a raw collection snapshot.

The example uses the bundled [synthetic legacy fixtures](../../tests/fixtures/legacy/). Its timestamps in January 1970 are intentional fixture values. The numbers, findings and modeled capacity do not describe a live validator. Missing evidence remains visible, and temporal comparison explicitly assumes legacy same-host continuity.

The screenshots in [the README](../../README.md) are captures of this HTML in Microsoft Edge at a 1440 × 1000 desktop viewport. The capacity detail captures its actual section, and the full-page image includes all report sections with findings initially collapsed. No values or statuses were changed for the screenshots.

To reproduce, build the CLI and create an unused output directory. From the repository root in a POSIX shell:

```sh
mkdir -p reports/readme-preview
cargo run --locked -- report compare tests/fixtures/legacy/snapshot-a.json tests/fixtures/legacy/snapshot-b.json --mode temporal --allow-legacy-identity --profile v10.4.0-mocha --out reports/readme-preview/report.html
python scripts/verify-report.py reports/readme-preview/report.html --browser /path/to/chromium --out reports/readme-preview/screenshots --full-page
```

The browser check requires Python with Playwright and an installed Chromium browser on the development machine. Replace `/path/to/chromium` with that browser's executable. It checks English report content, finding expansion, a 390px mobile viewport, JavaScript errors and external requests while offline. Python and Playwright are not needed to open the generated HTML or run the collector.

The published images are `report-en.png`, `report-capacity-en.png` and `report-full-en.png`. The verification script also produces a mobile screenshot and a JSON verification result for local review.
