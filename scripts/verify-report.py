"""Verify offline report rendering with Playwright and a Chromium browser."""
import argparse
import json
from pathlib import Path
from playwright.sync_api import sync_playwright

parser = argparse.ArgumentParser()
parser.add_argument('report', type=Path)
parser.add_argument('--browser', required=True)
parser.add_argument('--out', type=Path, required=True)
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
    titles = {}
    for index, lang in enumerate(('en', 'zh-CN', 'ko', 'ja')):
        page.locator(f'button[data-lang="{lang}"]').click()
        assert page.locator('html').get_attribute('lang') == lang
        assert page.locator(f'button[data-lang="{lang}"]').get_attribute('aria-pressed') == 'true'
        titles[lang] = page.locator('h1').inner_text()
        body = page.locator('body').inner_text()
        if interval is not None:
            assert f'{float(interval):.3f} s' in body
        for service in report['comparison']['services']:
            assert dictionary[service['identity_match']][index] in body
        assert 'undefined' not in body and 'NaN' not in body
        assert not page.evaluate("() => [...document.querySelectorAll('.reason')].some(n => n.textContent.startsWith('reason.'))")
        page.screenshot(path=str(args.out / f'report-{lang}.png'), full_page=False)
    assert len(set(titles.values())) == 4
    page.locator('[data-lang="en"]').click()
    if page.locator('details.finding').count():
        page.locator('details.finding').first.locator('summary').click()
        assert page.locator('details.finding').first.get_attribute('open') is not None
    page.set_viewport_size({'width': 390, 'height': 844})
    assert page.evaluate('document.documentElement.scrollWidth <= document.documentElement.clientWidth'), 'mobile horizontal overflow'
    page.screenshot(path=str(args.out / 'report-mobile.png'), full_page=False)
    assert not errors, errors
    assert not external, external
    result = {'passed': True, 'languages': titles, 'page_errors': errors, 'network_requests': external, 'offline': True, 'mobile_width': 390}
    (args.out / 'browser-results.json').write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(result, ensure_ascii=True, indent=2))
    browser.close()
