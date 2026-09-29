// Navigation/validation fixtures have no audio backend. Network is forbidden
// except for the local language catalogs read from this repository.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { JSDOM } = require('jsdom');

const views = ['routing', 'translation', 'transcription', 'recording', 'commands', 'settings'];
async function workspace(t, options = {}) {
  const html = `<!doctype html><html><body>
    <a class="skip-link" id="skip-link" href="#workspace-routing" data-workspace-target="routing">Skip to controls</a>
    <nav aria-label="Workspace">${views.map(view => `<button id="nav-${view}" class="nav-item" data-workspace-target="${view}"><span>${view}</span></button>`).join('')}</nav>
    <main><h1 id="page-title">Audio routing</h1><p id="page-description"></p><div id="error" hidden></div>
    <form id="configuration"><fieldset id="settings">
      <section id="workspace-routing" class="workspace-panel" data-workspace-panel="routing"><label><span>Physical microphone</span><select id="physical-mic"><option value="mic1">Mic one</option><option value="mic2">Mic two</option></select></label><button type="button" id="voice-shortcut" data-workspace-target="voices">Choose a voice</button><button type="button" id="mic-provider-shortcut" data-workspace-target="translation" data-workspace-field="microphone-provider">Microphone provider</button></section>
      <section id="workspace-translation" class="workspace-panel" data-workspace-panel="translation"><label><span>Microphone language</span><input id="mic-language" required value="pt-BR"></label><textarea id="voice-style">Calm and clear</textarea><select id="microphone-provider"><option value="gemini">Gemini</option><option value="openai">OpenAI</option></select><button type="button" id="profile-shortcut" data-workspace-target="translation" data-workspace-field="profile-selector">Provider settings</button><select id="profile-selector"><option value="gemini">Gemini</option><option value="openai">OpenAI</option></select><details id="provider-details"><summary>Connection</summary><section class="provider-profile" id="profile-gemini"><input id="gemini-endpoint" value="https://example.test"></section><section class="provider-profile" id="profile-openai" hidden><label><span>OpenAI endpoint</span><input id="openai-endpoint" type="url" required value="https://api.example.test"></label></section></details></section>
      <section id="workspace-transcription" class="workspace-panel" data-workspace-panel="transcription"><label><span>Transcript folder</span><input id="transcription-directory" value="transcripts"></label><button type="button" id="transcript-folder-shortcut" data-workspace-target="settings" data-workspace-field="files-base_path">Common folder</button></section>
      <section id="workspace-recording" class="workspace-panel" data-workspace-panel="recording"><input id="recording-directory" value="recordings"><button type="button" id="recording-folder-shortcut" data-workspace-field="files-base_path">Common folder</button></section>
      <section id="workspace-settings-files" class="workspace-panel" data-workspace-panel="settings"><label><span>Base folder</span><input id="files-base_path" required value="/home/test/Babel"></label><details id="file-details"><summary>Files</summary><label><span>Filename pattern</span><input id="file-pattern" required value="{session}-{id}"></label></details></section>
    </fieldset></form>
    <section id="workspace-commands" class="workspace-panel" data-workspace-panel="commands"><form id="agent-form"><details id="agent-details"><summary>Local service</summary><label><span>Speech service</span><input id="agent-endpoint" type="url" required value="http://127.0.0.1:8080"></label></details></form></section>
    <section id="workspace-settings" class="workspace-panel" data-workspace-panel="settings"><label><span>Start at login</span><input id="autostart-enabled" type="checkbox"></label></section>
    </main><section class="session-dock">Session controls</section><dialog id="voice-library"><div class="dialog-header"><h2>Voice library</h2></div><input id="voice-upload" type="file"><textarea id="voice-description"></textarea></dialog>
    </body></html>`;
  const dom = new JSDOM(html, { url: 'http://127.0.0.1:9473/#token=fixture', runScripts: 'outside-only', pretendToBeVisual: true });
  t.after(() => dom.window.close());
  const { window } = dom; const doc = window.document; const byId = id => doc.getElementById(id); const requests = [];
  window.fetch = async url => {
    requests.push(url); assert.match(url, /^\/locales\/(en|pt)\.json$/);
    return new Response(fs.readFileSync(path.join(__dirname, url), 'utf8'), { headers: { 'Content-Type': 'application/json' } });
  };
  if (options.storageThrows) Object.defineProperty(window, 'sessionStorage', { get() { throw new Error('Storage disabled'); } });
  else if (options.stored) window.sessionStorage.setItem('babel-workspace-view', options.stored);
  options.setup?.(window);
  byId('profile-selector').addEventListener('change', () => { for (const name of ['gemini', 'openai']) byId(`profile-${name}`).hidden = byId('profile-selector').value !== name; });
  window.eval(fs.readFileSync(path.join(__dirname, 'i18n.js'), 'utf8'));
  if (!options.beforeLanguage) await window.BabelI18n.setLanguage(options.language || 'en');
  window.eval(fs.readFileSync(path.join(__dirname, 'workspace.js'), 'utf8'));
  const nav = view => doc.querySelector(`.nav-item[data-workspace-target="${view}"]`);
  return { window, doc, byId, nav, requests, api: window.BabelWorkspace, panes: view => [...doc.querySelectorAll(`[data-workspace-panel="${view}"]`)] };
}

test('six views switch immediately without backend requests or remounting controls, uploads and dialogs', async t => {
  const p = await workspace(t, { beforeLanguage: true });
  assert.equal(p.api.current, 'routing'); assert.equal(p.byId('page-title').textContent, 'Audio routing');
  const input = p.byId('mic-language'); const style = p.byId('voice-style'); const file = p.byId('voice-upload');
  input.value = 'ja-JP'; p.byId('physical-mic').value = 'mic2'; style.value = 'My unsaved style';
  const recording = new p.window.File(['fixture'], 'voice.wav', { type: 'audio/wav' });
  Object.defineProperty(file, 'files', { value: [recording] });
  p.byId('voice-library').open = true; p.byId('voice-description').value = 'Custom voice';
  for (const view of views) {
    p.nav(view).click(); assert.equal(p.api.current, view);
    assert.equal(p.doc.querySelectorAll('.nav-item[aria-current="page"]').length, 1);
    assert.equal(p.nav(view).getAttribute('aria-current'), 'page');
    assert.ok(p.panes(view).every(panel => !panel.hidden));
    assert.ok([...p.doc.querySelectorAll('[data-workspace-panel]')].filter(panel => panel.dataset.workspacePanel !== view).every(panel => panel.hidden));
  }
  assert.equal(p.byId('mic-language'), input); assert.equal(input.value, 'ja-JP');
  assert.equal(p.byId('physical-mic').value, 'mic2'); assert.equal(style.value, 'My unsaved style');
  assert.equal(p.byId('voice-upload'), file); assert.equal(file.files[0], recording);
  assert.equal(p.byId('voice-library').open, true); assert.equal(p.byId('voice-description').value, 'Custom voice');
  assert.equal(p.byId('settings').disabled, false); assert.equal(p.requests.length, 0);
  assert.equal(p.window.location.hash, '#token=fixture');
});

test('navigation localizes the current heading and description without reverting after a catalog apply', async t => {
  const p = await workspace(t);
  p.nav('recording').click(); assert.equal(p.byId('page-title').textContent, 'Recording');
  const input = p.byId('recording-directory'); input.value = 'unsaved-audio'; input.focus();
  await p.window.BabelI18n.setLanguage('pt');
  assert.equal(p.api.current, 'recording');
  assert.equal(p.byId('page-title').textContent, p.window.BabelI18n.t('workspace.recording_title'));
  assert.equal(p.byId('page-description').textContent, p.window.BabelI18n.t('workspace.recording_description'));
  assert.equal(p.doc.activeElement, input); assert.equal(input.value, 'unsaved-audio');
  p.window.BabelI18n.apply(); assert.equal(p.byId('page-title').textContent, p.window.BabelI18n.t('workspace.recording_title'));
  p.nav('commands').click(); assert.equal(p.byId('page-title').textContent, 'Comandos de voz');
  await p.window.BabelI18n.setLanguage('en'); assert.equal(p.byId('page-title').textContent, 'Voice commands');
});

test('keyboard arrows and Home/End navigate normal buttons with aria-current and pane associations', async t => {
  const p = await workspace(t); p.nav('routing').focus();
  const key = value => p.doc.activeElement.dispatchEvent(new p.window.KeyboardEvent('keydown', { key: value, bubbles: true, cancelable: true }));
  key('ArrowDown'); assert.equal(p.api.current, 'translation'); assert.equal(p.doc.activeElement, p.nav('translation'));
  key('End'); assert.equal(p.api.current, 'settings');
  key('ArrowRight'); assert.equal(p.api.current, 'routing');
  key('ArrowLeft'); assert.equal(p.api.current, 'settings');
  key('Home'); assert.equal(p.api.current, 'routing');
  for (const button of p.doc.querySelectorAll('.nav-item')) {
    assert.equal(button.type, 'button');
    for (const id of button.getAttribute('aria-controls').split(' ')) assert.ok(p.byId(id));
  }
  assert.equal(p.nav('settings').getAttribute('aria-controls'), 'workspace-settings-files workspace-settings');
  p.nav('translation').focus(); key('ArrowDown'); assert.equal(p.api.current, 'transcription');
  key('ArrowDown'); assert.equal(p.api.current, 'recording');
});

test('internal shortcuts and saved views work when storage is available, denied or invalid', async t => {
  const p = await workspace(t); p.byId('voice-shortcut').focus(); p.byId('voice-shortcut').click();
  assert.equal(p.api.current, 'translation'); assert.equal(p.doc.activeElement, p.byId('page-title'));
  assert.equal(p.window.sessionStorage.getItem('babel-workspace-view'), 'translation');
  let scrolls = 0; p.doc.documentElement.scrollTo = options => { assert.equal(options.top, 0); scrolls++; };
  p.byId('skip-link').focus(); p.byId('skip-link').click();
  assert.equal(p.api.current, 'routing'); assert.equal(p.doc.activeElement, p.byId('page-title'));
  assert.equal(p.window.location.hash, '#token=fixture'); assert.equal(scrolls, 1);
  p.nav('recording').click(); assert.equal(scrolls, 2);
  delete p.doc.documentElement.scrollTo; p.doc.documentElement.scrollTop = 720;
  p.nav('settings').click(); assert.equal(p.doc.documentElement.scrollTop, 0);
  const saved = await workspace(t, { stored: 'commands' }); assert.equal(saved.api.current, 'commands');
  const invalid = await workspace(t, { stored: 'admin' }); assert.equal(invalid.api.current, 'routing');
  const denied = await workspace(t, { storageThrows: true }); denied.nav('settings').click(); assert.equal(denied.api.current, 'settings');
});

test('legacy stored views and navigate aliases migrate to canonical views without changing drafts', async t => {
  for (const [legacy, canonical] of [['audio', 'routing'], ['voices', 'translation'], ['files', 'recording']]) {
    const p = await workspace(t, { stored: legacy, beforeLanguage: true });
    assert.equal(p.api.current, canonical);
    assert.equal(p.window.sessionStorage.getItem('babel-workspace-view'), canonical);
    assert.equal(p.nav(canonical).getAttribute('aria-current'), 'page');
    p.byId('voice-style').value = 'Preserve voice draft';
    p.api.navigate('settings');
    assert.equal(p.api.navigate(legacy), true);
    assert.equal(p.api.current, canonical);
    assert.equal(p.byId('voice-style').value, 'Preserve voice draft');
    assert.equal(p.requests.length, 0);
  }
  const p = await workspace(t, { stored: 'transcription' });
  assert.equal(p.api.current, 'transcription');
  assert.equal(p.api.navigate('unknown'), false); assert.equal(p.api.current, 'transcription');
});

test('field shortcuts reveal and focus the actual control even within the current view or split Settings panes', async t => {
  const p = await workspace(t, { beforeLanguage: true });
  const targets = ['microphone-provider', 'profile-selector', 'files-base_path'];
  const scrolls = [];
  for (const id of targets) p.byId(id).scrollIntoView = options => { assert.equal(options.block, 'center'); scrolls.push(id); };
  let topScrolls = 0; p.doc.documentElement.scrollTo = () => { topScrolls++; };
  const voice = p.byId('voice-style'); voice.value = 'Unsaved voice';
  p.byId('mic-provider-shortcut').click();
  assert.equal(p.api.current, 'translation'); assert.equal(p.doc.activeElement, p.byId('microphone-provider'));
  p.byId('profile-shortcut').click();
  assert.equal(p.api.current, 'translation'); assert.equal(p.doc.activeElement, p.byId('profile-selector'));
  p.api.navigate('transcription'); p.byId('transcript-folder-shortcut').click();
  assert.equal(p.api.current, 'settings'); assert.equal(p.doc.activeElement, p.byId('files-base_path'));
  assert.equal(p.byId('workspace-settings-files').hidden, false); assert.equal(p.byId('workspace-settings').hidden, false);
  p.api.navigate('recording'); p.byId('recording-folder-shortcut').click();
  assert.equal(p.api.current, 'settings'); assert.equal(p.doc.activeElement, p.byId('files-base_path'));
  assert.deepEqual(scrolls, ['microphone-provider', 'profile-selector', 'files-base_path', 'files-base_path']);
  assert.equal(topScrolls, 0, 'field shortcuts must scroll to their target rather than reset the page');
  assert.equal(p.byId('voice-style'), voice); assert.equal(voice.value, 'Unsaved voice');
  assert.equal(p.requests.length, 0);
});

test('field shortcuts reveal hidden provider details and tolerate missing targets without enabling controls', async t => {
  const p = await workspace(t, { beforeLanguage: true });
  const trigger = p.byId('mic-provider-shortcut');
  trigger.dataset.workspaceField = 'openai-endpoint'; trigger.dataset.workspaceTarget = 'settings';
  trigger.click();
  assert.equal(p.api.current, 'translation', 'the real owning panel takes precedence over a stale target hint');
  assert.equal(p.byId('profile-selector').value, 'openai'); assert.equal(p.byId('profile-openai').hidden, false);
  assert.equal(p.byId('provider-details').open, true); assert.equal(p.doc.activeElement, p.byId('openai-endpoint'));
  trigger.dataset.workspaceField = 'removed-control'; trigger.dataset.workspaceTarget = 'files';
  trigger.click(); assert.equal(p.api.current, 'recording');
  trigger.dataset.workspaceTarget = 'unknown'; trigger.click(); assert.equal(p.api.current, 'recording');
  p.byId('settings').disabled = true;
  p.byId('autostart-enabled').checked = true;
  p.api.revealField(p.byId('files-base_path'));
  assert.equal(p.api.current, 'settings'); assert.equal(p.byId('settings').disabled, true);
  assert.equal(p.byId('files-base_path').matches(':disabled'), true);
  assert.equal(p.byId('autostart-enabled').matches(':disabled'), false);
  assert.equal(p.byId('autostart-enabled').checked, true);
  assert.equal(p.requests.length, 0);
});

test('save validation reveals only the first invalid field, including its details, without changing drafts', async t => {
  const p = await workspace(t);
  p.byId('mic-language').value = ''; p.byId('file-pattern').value = ''; p.byId('voice-style').value = 'Keep this';
  p.nav('settings').click();
  assert.equal(p.api.reportValidity(p.byId('configuration')), false);
  assert.equal(p.api.current, 'translation'); assert.equal(p.doc.activeElement, p.byId('mic-language'));
  assert.equal(p.byId('workspace-validation').hidden, false); assert.match(p.byId('workspace-validation').textContent, /Microphone language/);
  assert.equal(p.byId('mic-language').getAttribute('aria-invalid'), 'true');
  p.byId('mic-language').value = 'pt-BR';
  assert.equal(p.api.reportValidity(p.byId('configuration')), false);
  assert.equal(p.api.current, 'settings'); assert.equal(p.byId('file-details').open, true); assert.equal(p.doc.activeElement, p.byId('file-pattern'));
  assert.equal(p.byId('voice-style').value, 'Keep this');
  p.byId('file-pattern').value = '{id}'; p.byId('file-pattern').dispatchEvent(new p.window.Event('input', { bubbles: true }));
  assert.equal(p.byId('workspace-validation').hidden, true); assert.equal(p.byId('file-pattern').hasAttribute('aria-invalid'), false);
  assert.equal(p.api.reportValidity(p.byId('configuration')), true);
});

test('validation selects a hidden provider profile before native focus and keeps automatic invalid events in one view', async t => {
  const p = await workspace(t);
  p.byId('openai-endpoint').value = 'not a URL';
  assert.equal(p.api.reportValidity(p.byId('configuration')), false);
  assert.equal(p.api.current, 'translation'); assert.equal(p.byId('profile-selector').value, 'openai');
  assert.equal(p.byId('profile-openai').hidden, false); assert.equal(p.byId('provider-details').open, true);
  assert.equal(p.doc.activeElement, p.byId('openai-endpoint'));
  p.byId('mic-language').value = ''; p.byId('file-pattern').value = '';
  const one = new p.window.Event('invalid', { cancelable: true }); const two = new p.window.Event('invalid', { cancelable: true });
  p.byId('mic-language').dispatchEvent(one); p.byId('file-pattern').dispatchEvent(two);
  assert.equal(one.defaultPrevented, true); assert.equal(two.defaultPrevented, true);
  assert.equal(p.api.current, 'translation'); assert.equal(p.doc.activeElement, p.byId('mic-language'));
});

test('validation opens transcription and recording separately and reveals common file settings in both Settings panes', async t => {
  const p = await workspace(t);
  for (const [id, view] of [['transcription-directory', 'transcription'], ['recording-directory', 'recording'], ['files-base_path', 'settings']]) {
    const field = p.byId(id); const previous = field.value;
    field.required = true; field.value = '';
    p.api.navigate('routing');
    assert.equal(p.api.reportValidity(p.byId('configuration')), false);
    assert.equal(p.api.current, view); assert.equal(p.doc.activeElement, field);
    assert.equal(field.closest('[data-workspace-panel]').hidden, false);
    field.value = previous; field.dispatchEvent(new p.window.Event('input', {bubbles:true}));
  }
  assert.equal(p.panes('settings').length, 2); assert.ok(p.panes('settings').every(pane => !pane.hidden));
  assert.equal(p.api.reportValidity(p.byId('configuration')), true);
});

test('agent validation and modal validation open the right controls without closing the voice library', async t => {
  const p = await workspace(t, { language: 'pt' });
  p.byId('agent-endpoint').value = '';
  const invalid = new p.window.Event('invalid', { cancelable: true }); p.byId('agent-endpoint').dispatchEvent(invalid);
  assert.equal(p.api.current, 'commands'); assert.equal(p.byId('agent-details').open, true);
  assert.equal(p.doc.activeElement, p.byId('agent-endpoint')); assert.match(p.byId('workspace-validation').textContent, /Revise/);
  p.byId('voice-library').open = true; p.byId('voice-description').required = true;
  assert.equal(p.api.reportValidity(p.byId('voice-description')), false);
  assert.equal(p.byId('voice-library').open, true);
  assert.equal(p.byId('workspace-validation-voice-library').hidden, false);
  assert.equal(p.doc.activeElement, p.byId('voice-description'));
});

test('the session dock reserves its measured height and observes only size changes without touching drafts', async t => {
  let height = 100.25; let callback; const observed = [];
  const p = await workspace(t, { setup(window) {
    window.document.querySelector('.session-dock').getBoundingClientRect = () => ({ height });
    window.ResizeObserver = class { constructor(fn) { callback = fn; } observe(node) { observed.push(node); } };
  } });
  const style = p.doc.documentElement.style;
  assert.equal(style.getPropertyValue('--session-dock-height'), '101px');
  assert.deepEqual(observed, [p.doc.querySelector('.session-dock')]);
  p.byId('voice-style').value = 'Unsaved voice'; p.nav('recording').click();
  let writes = 0; const setProperty = style.setProperty.bind(style);
  style.setProperty = (...args) => { writes++; return setProperty(...args); };
  callback(); assert.equal(writes, 0);
  height = 210.5; callback();
  assert.equal(style.getPropertyValue('--session-dock-height'), '211px'); assert.equal(writes, 1);
  callback(); assert.equal(writes, 1);
  assert.equal(p.byId('voice-style').value, 'Unsaved voice'); assert.equal(p.api.current, 'recording');
  const fallback = await workspace(t);
  assert.equal(fallback.doc.documentElement.style.getPropertyValue('--session-dock-height'), '');
  fallback.doc.querySelector('.session-dock').getBoundingClientRect = () => ({ height: 150 });
  fallback.window.dispatchEvent(new fallback.window.Event('resize'));
  assert.equal(fallback.doc.documentElement.style.getPropertyValue('--session-dock-height'), '150px');
});

test('workspace captions exist in English and Portuguese for every view', () => {
  const en = JSON.parse(fs.readFileSync(path.join(__dirname, 'locales/en.json'), 'utf8'));
  const pt = JSON.parse(fs.readFileSync(path.join(__dirname, 'locales/pt.json'), 'utf8'));
  for (const view of views) for (const suffix of ['nav', 'title', 'description']) {
    assert.equal(typeof en[`workspace.${view}_${suffix}`], 'string'); assert.equal(typeof pt[`workspace.${view}_${suffix}`], 'string');
  }
});
