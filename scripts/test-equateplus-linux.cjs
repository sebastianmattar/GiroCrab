// The Linux EquatePlus flow (GiroCrabLinux/qml/EquatePlusWebLoginFlow.qml) runs exactly the
// scripts of EquatePlusUI.swift, and they work when run as the Linux browser sheet runs them
// (page world, `runAsync` = an async function body). Synthetic data only; the full script
// tests are GiroCrabApp/Tools/test-equateplus.mjs.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const {webcrypto} = require('node:crypto');

const {pathToFileURL} = require('node:url');
const read = p => readFileSync(new URL(p, pathToFileURL(__filename)), 'utf8');
const swift = read('../GiroCrabApp/Sources/GiroCrabUI/EquatePlusUI.swift');
const qml = read('../GiroCrabLinux/qml/EquatePlusWebLoginFlow.qml');
const linux = Object.fromEntries([...qml.matchAll(/readonly property string (\w+): `((?:\\[\s\S]|[^`\\])*)`/g)]
  .map(m => [m[1], vm.runInNewContext('`' + m[2] + '`')]));
const dedent = (s, n) => s.split('\n').map(l => l.startsWith(' '.repeat(n)) ? l.slice(n) : l.trimStart()).join('\n');

test('the Linux flow runs exactly the scripts of the Swift file', () => {
  const hook = swift.match(/static let hook = #"""\n([\s\S]*?)\n    """#/)[1];
  const guard = swift.match(/let guardScript = """\n([\s\S]*?)\n        """/)[1];
  const probe = swift.match(/static let probe = "([^"\n]*)"/)[1];
  const collect = swift.match(/callAsyncJavaScript\(\n\s*"([^"\n]*collect\(\);)"/)[1];
  assert.deepEqual(Object.keys(linux).sort(), ['collect', 'guard', 'hook', 'probe']);
  const expected = {hook: dedent(hook, 4), guard: dedent(guard, 8), probe, collect};
  for (const name of Object.keys(expected))
    assert.equal(linux[name], expected[name], `${name} differs: run python3 -I scripts/gen-equateplus-qml.py`);
});

const profile = {participant: {userId: 'test-user'}, reportingCurrency: {code: 'EUR'}, privateAddress: 'do not export'};
function portal(host = 'www.equateplus.com') {
  const calls = [];
  const document = {scripts: [{textContent: 'csrfRegisterAjax("csrfpId", "bootstrap-csrf");'}], login: false,
    querySelector(selector) { return selector === '#loginForm' && this.login ? {} : null; }};
  class XHR { open() {} setRequestHeader() {} addEventListener() {} }
  const json = data => new Response(JSON.stringify(data), {headers: {'content-type': 'application/json'}});
  const context = vm.createContext({URL, Headers, Request, Response, AbortController, crypto: webcrypto, setTimeout, clearTimeout, btoa,
    location: new URL('https://' + host + '/EquatePlusParticipant2/'), document, XMLHttpRequest: XHR,
    fetch: async (url, init = {}) => {
      const u = new URL(url, 'https://' + host);
      calls.push({u, init});
      if (u.pathname.endsWith('user/get')) return json(profile);
      if (u.pathname.endsWith('planSummary/get')) return json({entries: [{id: 'test-plan', name: 'Test Programm'}]});
      if (u.pathname.endsWith('planDetails/get')) return json({name: 'Test Programm', secret: 'do not export',
        totals: [{type: 'TOTAL', quantity: {amount: 1.25}, value: {amount: 125}}],
        entries: [{vehicle: 'TEST', productType: 'RSU', marketPrice: {amount: 100, unit: {code: 'EUR'}}, canTrade: false,
          entries: [{VEHICLE: 'Test RSU', LOCKED_QTY: {amount: 1.25}}]}]});
      if (u.pathname.endsWith('documents/library')) return json({documents: [{id: 'doc1', date: '2011-05-26T00:00:00',
        description: 'Test', documentType: 'Dividendenbestätigung', plan: null, url: '/services/statements/download?documentId=doc1'}]});
      if (u.pathname.endsWith('/download')) return new Response(new TextEncoder().encode('%PDF-1.4 test'), {headers: {'content-type': 'application/pdf'}});
      return json({ok: true});
    }});
  vm.runInContext('window = globalThis', context);
  // WebLoginSheet.addScript: user script at document creation in the main (page) world
  vm.runInContext(linux.hook, context);
  // WebLoginSheet.runAsync: the body runs as an async function with the arguments as parameters
  const runAsync = (body, args = {}) => vm.runInContext(
    `(async function(${Object.keys(args).join(',')}){\n${body}\n})(${Object.values(args).map(v => JSON.stringify(v)).join(',')})`, context);
  return {runAsync, document, calls};
}

test('probe, holdings and documents as the Linux flow calls them', async () => {
  const {runAsync, document, calls} = portal();
  document.login = true;
  assert.equal(await runAsync(linux.probe), false, 'no probe on a login or 2FA form');
  assert.equal(calls.length, 0);
  document.login = false;
  assert.equal(await runAsync(linux.probe), true);
  const snapshot = await runAsync(linux.collect);
  assert.equal(typeof snapshot, 'string');
  assert(!snapshot.includes('do not export') && !snapshot.includes('bootstrap-csrf'));
  assert.equal(JSON.parse(snapshot).plans[0].details.entries[0].entries[0].LOCKED_QTY.amount, 1.25);
  const list = JSON.parse(await runAsync(linux.guard + 'return await window.__gcEquatePlus.documents();'));
  const pdf = await runAsync(linux.guard + 'return await window.__gcEquatePlus.pdf(url);', {url: list[0].url});
  assert.equal(Buffer.from(pdf, 'base64').toString('latin1'), '%PDF-1.4 test');
});

test('the guard refuses a page that is not signed in', async () => {
  const {runAsync} = portal();
  await assert.rejects(runAsync(linux.guard + 'return "x";'), /EquatePlus: Bitte erneut anmelden\./);
  await assert.rejects(runAsync(linux.collect), /EquatePlus: Bitte erneut anmelden\./);
});

test('the hook stays out of foreign hosts', async () => {
  for (const host of ['evil.example', 'www.equateplus.com.evil.example']) {
    const {runAsync} = portal(host);
    assert.equal(await runAsync('return typeof window.__gcEquatePlus;'), 'undefined');
    await assert.rejects(runAsync(linux.guard + 'return "x";'), /Bitte erneut anmelden/);
  }
});
