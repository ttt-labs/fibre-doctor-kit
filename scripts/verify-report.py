"""Verify offline report rendering with Playwright and a Chromium browser."""
import argparse
import json
from pathlib import Path
from playwright.sync_api import sync_playwright

parser = argparse.ArgumentParser()
parser.add_argument('report', type=Path)
parser.add_argument('--browser', required=True)
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--full-page', action='store_true', help='Also save the complete English report and capacity detail')
args = parser.parse_args()
args.out.mkdir(parents=True, exist_ok=True)
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path=args.browser, headless=True)
    context = browser.new_context(offline=True, viewport={'width': 1440, 'height': 1000})
    page = context.new_page()
    errors, external = [], []
    page.on('pageerror', lambda error: errors.append(str(error) + '\n' + (error.stack or '')))
    page.on('request', lambda request: external.append(request.url) if request.url.startswith(('http:', 'https:')) else None)
    page.goto(args.report.resolve().as_uri())
    assert not errors, errors
    report = json.loads(page.locator('#report-data').text_content())
    dictionary = json.loads(page.locator('#translations').text_content())
    interval = report['comparison']['elapsed_seconds']
    assert page.locator('html').get_attribute('lang') == 'en'
    assert page.locator('[data-lang]').count() == 0
    assert page.locator('.brand').inner_text() == 'DOCTOR FIBRE KIT'
    assert page.title().startswith('Doctor Fibre Kit')
    assert all(len(values) == 1 for values in dictionary.values())
    title = page.locator('h1').inner_text()
    assert title == dictionary['title'][0]
    body = page.locator('body').inner_text()
    if interval is not None:
        assert f'{float(interval):.3f} s' in body
    for service in report['comparison']['services']:
        assert dictionary[service['identity_match']][0] in body
    assert 'undefined' not in body and 'NaN' not in body
    assert not page.evaluate("() => [...document.querySelectorAll('.reason')].some(n => n.textContent.startsWith('reason.'))")
    assert dictionary['fd_note'][0] in body
    page.screenshot(path=str(args.out / 'report-en.png'), full_page=False)
    if args.full_page:
        page.screenshot(path=str(args.out / 'report-full-en.png'), full_page=True)
        capacity = page.locator('section').filter(has=page.get_by_role('heading', name=dictionary['capacity_title'][0], exact=True))
        capacity.screenshot(path=str(args.out / 'report-capacity-en.png'))
    if page.locator('details.finding').count():
        page.locator('details.finding').first.locator('summary').click()
        assert page.locator('details.finding').first.get_attribute('open') is not None
    page.set_viewport_size({'width': 390, 'height': 844})
    assert page.evaluate('document.documentElement.scrollWidth <= document.documentElement.clientWidth'), 'mobile horizontal overflow'
    page.screenshot(path=str(args.out / 'report-mobile.png'), full_page=False)
    assert not errors, errors
    assert not external, external
    result = {'passed': True, 'language': 'en', 'title': title, 'page_errors': errors, 'network_requests': external, 'offline': True, 'mobile_width': 390}
    (args.out / 'browser-results.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(result, ensure_ascii=True, indent=2))
    browser.close()
