// Development-only DOM regressions: run npm ci && npm test in ui/.
// The shipped dashboard remains dependency-free. Every HTTP response here is an in-memory fixture.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { JSDOM } = require('jsdom');

function defaults() {
  const cloud = (values = {}) => ({ api_key_env: 'GEMINI_API_KEY', endpoint: 'wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent', model: 'gemini-3.5-live-translate-preview', transcription_model: '', connect_timeout_secs: 15, max_reconnect_attempts: 5, ...values });
  const route = (values = {}) => ({ enabled: true, provider: 'gemini', capture_device: 'physical-mic', playback_device: 'babel_mic_bus', source_language: 'pt-BR', target_language: 'en-US', prompt: '', gain: 1, ...values });
  return {
    version: 1, interface: { language: 'system' }, local_runtime: { directory: '', threads: 2, idle_unload_secs: 60 },
    providers: { gemini: cloud(), openai: cloud({ api_key_env: 'OPENAI_API_KEY', endpoint: '', model: 'gpt-realtime-translate' }), local: { whisper_endpoint: 'auto', whisper_model: 'base', ollama_endpoint: 'auto', translation_api: 'ollama', translation_model: 'qwen3-0.6b', piper_endpoint: 'auto', segment_ms: 2000, silence_ms: 300, vad_threshold: 0.01, request_timeout_secs: 30 } },
    audio: { microphone_source: 'physical_microphone', quality: 'balanced', capture_queue_ms: 200, playback_queue_ms: 2000, max_capture_age_ms: 200, device_latency_ms: 30 },
    microphone: route(), speaker: route({ capture_device: 'babel_speaker.monitor', playback_device: 'physical-speaker', source_language: 'en-US', target_language: 'pt-BR' }),
    transcription: { enabled: false, microphone: true, speaker: true, timestamps: true, directory: 'transcripts',
      microphone_recognition: { provider: 'gemini', language: 'auto' }, speaker_recognition: { provider: 'gemini', language: 'auto' },
      providers: {
        gemini: { api_key_env: 'GEMINI_API_KEY', endpoint: cloud().endpoint, model: 'gemini-3.5-transcribe-live', connect_timeout_secs: 15, max_reconnect_attempts: 5 },
        openai: { api_key_env: 'OPENAI_API_KEY', endpoint: '', model: 'gpt-live-transcribe', connect_timeout_secs: 15, max_reconnect_attempts: 5 },
        deepgram: { api_key_env: 'DEEPGRAM_API_KEY', endpoint: 'wss://api.deepgram.com/v1/listen', model: 'nova-3', connect_timeout_secs: 15, max_reconnect_attempts: 3, diarize: true, punctuate: true },
        whisper: { endpoint: 'auto', model: 'base', api_key_env: '', segment_ms: 2000, silence_ms: 300, vad_threshold: 0.01, request_timeout_secs: 30 },
      } },
    files: { base_path: '/home/test/Babel', name_pattern: '{date}-{time}-{session}-{id}' },
    recording: { enabled: false, microphone: true, speaker: true, directory: 'recordings', mix: { microphone_gain_db: 0, speaker_gain_db: 0, microphone_priority: true, ducking_db: 12, microphone_threshold_db: -50 } },
    history: { enabled: true, duration_secs: 600 },
  };
}
async function settle(predicate, message = 'condition was not reached') {
  for (let attempt = 0; attempt < 1200; attempt++) { if (predicate()) return; await new Promise(resolve => setTimeout(resolve, 1)); }
  assert.fail(message);
}
async function page(t, options = {}) {
  const dom = new JSDOM(fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8'), { url: `http://127.0.0.1:8765/#token=${'a'.repeat(64)}`, runScripts: 'outside-only', pretendToBeVisual: true });
  t.after(async () => { await new Promise(resolve => setImmediate(resolve)); dom.window.close(); });
  const window = dom.window;
  const doc = window.document;
  const byId = id => doc.getElementById(id);
  const calls = [];
  let config = defaults();
  config.interface.language = options.language || 'system';
  const systemLocale = options.systemLocale || 'pt-BR';
  const localeFailure = options.localeFailure;
  const omitTranslation = options.omitTranslation;
  let platformFailure = options.platformFailure;
  const platformOs = options.platform || 'linux';
  config.files.base_path = options.basePath ?? (platformOs === 'windows' ? 'C:\\Users\\Test\\Babel' : '/home/test/Babel');
  if (options.missingBase) delete config.files.base_path;
  options.configure?.(config);
  let filePathsHandler = options.filePaths;
  const resolvePaths = body => {
    const hostPath = platformOs === 'windows' ? path.win32 : path.posix;
    assert.equal(hostPath.isAbsolute(body.base_path), true, 'relative bases must not reach the preview API');
    const base_path = hostPath.resolve(body.base_path);
    return { base_path, transcription_directory: hostPath.resolve(base_path, body.transcription_directory), recording_directory: hostPath.resolve(base_path, body.recording_directory) };
  };
  const platform = { os: platformOs, name: ({ linux: 'Linux', macos: 'macOS', windows: 'Windows' })[platformOs] || 'Unknown', audio_backend: ({linux:'pulseaudio',macos:'coreaudio',windows:'wasapi'})[platformOs] || 'unsupported', manages_virtual_devices: platformOs === 'linux', autostart_method: ({linux:'xdg',macos:'launch_agent',windows:'registry_run'})[platformOs] || 'unsupported' };
  if (options.userAgent) Object.defineProperty(window.navigator, 'userAgent', { value: options.userAgent });
  const interfaceMetadata = () => ({ language: config.interface.language, resolved_language: config.interface.language === 'system' ? systemLocale.toLowerCase().startsWith('pt') ? 'pt' : 'en' : config.interface.language, system_locale: systemLocale, languages: [{ code: 'en', name: 'English' }, { code: 'pt', name: 'Português' }], config_revision: revision });
  let revision = 0;
  let interval;
  let firstStatus = true;
  let running = false;
  let localRuntime = { phase: 'idle', message: null, download: null, services: [] };
  let audioHistory = { enabled: true, capacity_secs: 600, available_secs: 180, combined_audio_secs: 180, microphone_secs: 180, speaker_secs: 90, ...options.history };
  let historySession = { history_included_secs: 0, history_transcription_pending: false, ...options.historySession };
  let routingActive = true;
  let routingError = null;
  let sessionName = null;
  let sessionId = null;
  let autostart = false;
  const autostartMetadata = options.autostartMetadata || {};
  const credentials = new Set();
  const metrics = { state: 'stopped', captured_frames: 0, dropped_frames: 0, underruns: 0, translated_samples: 0, reconnects: 0, input_level: 0, output_level: 0, last_input_transcript: null };
  const routeStatuses = { microphone: {}, speaker: {} };
  window.structuredClone = structuredClone;
  window.AbortSignal = AbortSignal;
  window.setInterval = callback => { interval = callback; return 0; };
  window.HTMLDialogElement.prototype.showModal = function () { this.open = true; };
  window.HTMLDialogElement.prototype.close = function () { this.open = false; };
  const initialChange = options.initialChange;
  const startRequest = options.startRequest;
  const statusRequest = options.statusRequest;
  const extraInputDevices = options.extraInputDevices || [];
  window.fetch = async (url, options) => {
    const parsed = new URL(url, window.location.origin);
    assert.equal(parsed.pathname.startsWith('/api/voices'), false, 'the dashboard never calls removed voice APIs');
    if (parsed.pathname.startsWith('/locales/')) {
      if (localeFailure === parsed.pathname) return new Response('unavailable', { status: 500 });
      const catalog = JSON.parse(fs.readFileSync(path.join(__dirname, parsed.pathname), 'utf8'));
      if (omitTranslation && parsed.pathname.endsWith('/pt.json')) delete catalog[omitTranslation];
      return new Response(JSON.stringify(catalog), { headers: { 'Content-Type': 'application/json' } });
    }
    const body = options.body ? JSON.parse(options.body) : undefined;
    calls.push({ path: parsed.pathname, options, body });
    assert.equal(options.headers.Authorization, `Bearer ${'a'.repeat(64)}`);
    if (options.headers['If-Match'] !== undefined && options.headers['If-Match'] !== `"${revision}"`) return new Response(JSON.stringify({ error: 'The configuration changed. Reload settings.' }), { status: 412, headers: { 'Content-Type': 'application/json' } });
    let value = { ok: true };
    if (parsed.pathname === '/api/interface') { if (options.method === 'PUT') { config.interface.language = body.language; revision++; } value = interfaceMetadata(); }
    else if (parsed.pathname === '/api/platform') { if (platformFailure) return new Response(JSON.stringify({error:'Host metadata unavailable'}), {status:503,headers:{'Content-Type':'application/json'}}); value = platform; }
    else if (parsed.pathname === '/api/config' && options.method === 'GET') value = config;
    else if (parsed.pathname === '/api/config') { config = body; revision++; }
    else if (parsed.pathname === '/api/file-paths') {
      assert.equal(options.method, 'POST');
      value = filePathsHandler ? await filePathsHandler(body, options, resolvePaths) : resolvePaths(body);
      if (value instanceof Response) return value;
    }
    else if (parsed.pathname === '/api/devices') value = [{ id: 'physical-mic', name: 'Physical mic', direction: 'input', is_virtual: false }, { id: 'physical-speaker', name: 'Headphones', direction: 'output', is_virtual: false }, { id: 'babel_mic_bus', name: platformOs === 'macos' ? 'Babel Microphone' : platformOs === 'windows' ? 'Babel Microphone Feed' : 'Virtual microphone', direction: 'output', is_virtual: true }, { id: 'babel_speaker.monitor', name: platformOs === 'macos' ? 'Babel Speaker' : platformOs === 'windows' ? 'Babel Speaker Monitor' : 'Virtual output', direction: 'input', is_virtual: true }, ...extraInputDevices];
    else if (parsed.pathname === '/api/status') {
      if (statusRequest) await statusRequest();
      if (firstStatus && initialChange) { initialChange(config); revision++; }
      firstStatus = false;
      const route = name => ({ ...metrics, state: running && config[name].enabled ? 'streaming' : routingActive || running ? 'passthrough' : 'stopped', ...routeStatuses[name] });
      value = { ...historySession, history: audioHistory, local_runtime: localRuntime, running, routing_active: routingActive, routing_error: routingError, config_revision: revision, session_name: sessionName, session_id: sessionId, microphone: route('microphone'), speaker: route('speaker'), last_error: null };
    }
    else if (parsed.pathname === '/api/start') { if (startRequest) { const response = await startRequest(); if (response instanceof Response) return response; } running = true; sessionName = body?.name || 'Automatic session'; sessionId = 'fixture-session-id'; }
    else if (parsed.pathname === '/api/stop') running = false;
    else if (parsed.pathname === '/api/autostart') { if (options.method === 'POST') autostart = body.enabled; value = { ...autostartMetadata, enabled: autostart, supported: true, description: autostart ? 'Início automático ativado; tradução parada.' : 'Início automático desativado.' }; }
    else if (parsed.pathname === '/api/credentials' && options.method === 'GET') value = { configured: credentials.has(parsed.searchParams.get('api_key_env')) };
    else if (parsed.pathname === '/api/credentials') credentials.add(body.api_key_env);
    else if (parsed.pathname === '/api/credentials/clear') credentials.delete(body.api_key_env);
    return new Response(JSON.stringify(value), { headers: { 'Content-Type': 'application/json', ...(parsed.pathname === '/api/config' ? { ETag: `"${revision}"` } : {}) } });
  };
  window.eval(fs.readFileSync(path.join(__dirname, 'i18n.js'), 'utf8'));
  window.eval(fs.readFileSync(path.join(__dirname, 'app.js'), 'utf8'));
  window.eval(fs.readFileSync(path.join(__dirname, 'workspace.js'), 'utf8'));
  await settle(() => !byId('start').disabled && calls.some(call => call.path === '/api/platform'), 'dashboard did not initialize');
  const set = (id, value) => { const input = byId(id); if (input.type === 'checkbox') input.checked = value; else input.value = value; input.dispatchEvent(new window.Event('input', { bubbles: true })); };
  return { window, doc, byId, calls, set, history: status => { audioHistory = status; }, historySession: status => { historySession = status; }, runtime: status => { localRuntime = status; }, config: () => config, poll: () => interval(), externalChange: callback => { callback(config); revision++; }, filePaths: handler => { filePathsHandler = handler; }, platformFailure: failure => { platformFailure = failure; }, routing: (active, error = null) => { routingActive = active; routingError = error; }, routeStatus: (route, status) => { routeStatuses[route] = status; } };
}

test('the real dashboard separates routing, translation, transcription and recording into six workspace destinations', async t => {
  const p = await page(t);
  const views = ['routing', 'translation', 'transcription', 'recording', 'commands', 'settings'];
  assert.deepEqual([...p.doc.querySelectorAll('.nav-item[data-workspace-target]')].map(node => node.dataset.workspaceTarget), views);
  assert.deepEqual([...new Set([...p.doc.querySelectorAll('[data-workspace-panel]')].map(node => node.dataset.workspacePanel))].sort(), [...views].sort());
  const groups = {
    routing: ['microphone-capture_device', 'microphone-playback_device', 'speaker-capture_device', 'speaker-playback_device', 'microphone-input', 'speaker-output', 'microphone-state', 'speaker-state', 'microphone-signal', 'speaker-signal', 'audio-quality'],
    translation: ['microphone-enabled', 'speaker-enabled', 'microphone-source_language', 'speaker-target_language', 'microphone-provider', 'speaker-provider', 'microphone-prompt', 'speaker-prompt', 'microphone-gain', 'speaker-gain', 'profile-selector', 'credential-gemini'],
    transcription: ['transcription-enabled', 'transcription-microphone', 'transcription-speaker', 'transcription-timestamps', 'transcription-directory', 'microphone-transcripts', 'speaker-transcripts', 'stt-microphone-provider', 'stt-speaker-language', 'stt-profile-selector', 'stt-profile-deepgram-model', 'stt-profile-whisper-endpoint', 'stt-credential-gemini'],
    recording: ['recording-enabled', 'recording-microphone', 'recording-speaker', 'recording-directory', 'recording-microphone-gain', 'recording-speaker-gain', 'recording-microphone-priority', 'recording-ducking', 'recording-microphone-threshold'],
    settings: ['files-base_path', 'files-name_pattern', 'files-path-preview'],
  };
  for (const [view, ids] of Object.entries(groups)) {
    for (const id of ids) {
      const element = p.byId(id);
      assert.ok(element, `missing ${id}`);
      assert.equal(element.closest('[data-workspace-panel]').dataset.workspacePanel, view, `${id} belongs to ${view}`);
      assert.equal(element.closest('form'), p.byId('configuration'), `${id} must remain in the shared configuration form`);
    }
  }
  const routingFields = [...p.byId('workspace-routing').querySelectorAll('[data-field]')];
  assert.equal(routingFields.some(node => node.dataset.profile || ['enabled', 'provider', 'source_language', 'target_language', 'prompt', 'gain'].includes(node.dataset.field)), false, 'audio routing must not contain translation controls');
  assert.equal(new Set([...p.doc.querySelectorAll('[id]')].map(node => node.id)).size, p.doc.querySelectorAll('[id]').length, 'moving controls must not duplicate their IDs');
});

test('moving between feature pages preserves independent drafts and live transcript nodes without saving or changing the session', async t => {
  const p = await page(t);
  const before = structuredClone(p.config());
  p.byId('nav-translation').click();
  p.set('speaker-provider', 'openai');
  p.set('profile-openai-model', 'gpt-realtime-2.1');
  p.set('speaker-prompt', 'Preserve os termos técnicos.');
  p.set('speaker-target_language', 'ja-JP');
  p.set('microphone-enabled', false);
  p.set('speaker-enabled', false);
  p.byId('nav-transcription').click();
  p.set('transcription-enabled', true);
  p.set('transcription-speaker', false);
  p.set('transcription-timestamps', false);
  assert.equal(p.byId('recording-enabled').checked, false);
  p.byId('nav-recording').click();
  p.set('recording-enabled', true);
  p.set('recording-microphone', false);
  assert.equal(p.byId('transcription-enabled').checked, true);
  const preservedIds = ['speaker-target_language', 'speaker-prompt', 'transcription-enabled', 'recording-enabled', 'microphone-input-transcript'];
  const originalNodes = preservedIds.map(id => p.byId(id));
  p.routeStatus('microphone', { last_input_transcript: 'Texto original do microfone.' });
  await p.poll();
  for (const view of ['routing', 'settings', 'translation', 'commands', 'transcription', 'recording']) {
    p.byId(`nav-${view}`).click();
    assert.equal(p.window.BabelWorkspace.current, view);
    assert.equal([...p.doc.querySelectorAll('[data-workspace-panel]')].every(panel => panel.hidden === (panel.dataset.workspacePanel !== view)), true);
  }
  for (const [index, id] of preservedIds.entries()) assert.equal(p.byId(id), originalNodes[index]);
  assert.equal(p.byId('speaker-prompt').value, 'Preserve os termos técnicos.');
  assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
  assert.equal(p.byId('microphone-enabled').checked, false);
  assert.equal(p.byId('speaker-enabled').checked, false);
  assert.equal(p.byId('transcription-enabled').checked, true);
  assert.equal(p.byId('transcription-speaker').checked, false);
  assert.equal(p.byId('transcription-timestamps').checked, false);
  assert.equal(p.byId('recording-enabled').checked, true);
  assert.equal(p.byId('recording-microphone').checked, false);
  assert.equal(p.byId('microphone-input-transcript').textContent, 'Texto original do microfone.');
  assert.deepEqual(p.config(), before);
  assert.equal(p.calls.some(call => call.options.method === 'PUT' || ['/api/start', '/api/stop'].includes(call.path)), false);
  assert.equal(p.byId('save').disabled, false);
});

test('shared file settings keep the same draft preview after visiting both file pages and during a recording-only session', async t => {
  const p = await page(t);
  p.byId('nav-settings').click();
  const settings = p.byId('workspace-settings-files');
  assert.equal(settings.hidden, false);
  assert.equal(p.byId('workspace-settings').hidden, false);
  p.set('files-base_path', '/archive/My sessions');
  p.set('files-name_pattern', '{session}-{id}');
  p.byId('nav-transcription').click();
  p.set('transcription-directory', 'original-text');
  p.byId('nav-recording').click();
  p.set('recording-directory', 'original-audio');
  p.set('recording-enabled', true);
  p.byId('nav-translation').click();
  p.set('microphone-enabled', false);
  p.set('speaker-enabled', false);
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  p.byId('nav-settings').click();
  assert.equal(p.byId('files-resolved-base').textContent, '/archive/My sessions');
  assert.equal(p.byId('transcription-resolved-directory').textContent, '/archive/My sessions/original-text');
  assert.equal(p.byId('recording-resolved-directory').textContent, '/archive/My sessions/original-audio');
  assert.equal(p.byId('files-name_pattern').value, '{session}-{id}');
  assert.equal(p.config().files.base_path, '/home/test/Babel');
  assert.equal(p.calls.some(call => call.options.method === 'PUT' || ['/api/start', '/api/stop'].includes(call.path)), false);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.config().recording.enabled, true);
  assert.equal(p.config().transcription.enabled, false);
  assert.equal(p.config().microphone.enabled, false);
  assert.equal(p.config().speaker.enabled, false);
  const lifecycleCalls = p.calls.filter(call => call.options.method === 'PUT' || ['/api/start', '/api/stop'].includes(call.path)).length;
  for (const view of ['routing', 'translation', 'transcription', 'recording', 'commands', 'settings']) p.byId(`nav-${view}`).click();
  assert.equal(p.calls.filter(call => call.options.method === 'PUT' || ['/api/start', '/api/stop'].includes(call.path)).length, lifecycleCalls);
  assert.equal(p.byId('stop').hidden, false);
  assert.equal(p.byId('files-base_path').matches(':disabled'), true);
  assert.equal(p.byId('recording-resolved-directory').textContent, '/archive/My sessions/original-audio');
});

test('session feature switches mirror existing drafts and preserve the selected translation directions', async t => {
  const p = await page(t, { language: 'en' });
  const quick = feature => p.byId(`session-${feature}-enabled`);
  for (const feature of ['translation', 'recording', 'transcription']) {
    assert.equal(quick(feature).hasAttribute('data-field'), false, 'mirrors must not overwrite original configuration fields');
  }
  assert.equal(quick('translation').checked, true);
  assert.equal(quick('recording').checked, false);
  assert.equal(quick('transcription').checked, false);
  assert.equal(p.byId('session-translation-scope').textContent, 'Both directions');
  p.set('session-translation-enabled', false);
  assert.equal(p.byId('microphone-enabled').checked, false);
  assert.equal(p.byId('speaker-enabled').checked, false);
  p.set('session-translation-enabled', true);
  assert.equal(p.byId('microphone-enabled').checked, true);
  assert.equal(p.byId('speaker-enabled').checked, true, 'a master toggle must not remember an intermediate route state');
  p.set('speaker-enabled', false);
  assert.equal(quick('translation').checked, true);
  assert.equal(p.byId('session-translation-scope').textContent, 'Microphone only');
  const provider = p.byId('microphone-provider').value;
  const language = p.byId('microphone-target_language').value;
  p.set('session-translation-enabled', false);
  p.set('session-translation-enabled', true);
  assert.equal(p.byId('microphone-enabled').checked, true);
  assert.equal(p.byId('speaker-enabled').checked, false);
  assert.equal(p.byId('microphone-provider').value, provider);
  assert.equal(p.byId('microphone-target_language').value, language);
  p.set('recording-enabled', true);
  p.set('transcription-enabled', true);
  assert.equal(quick('recording').checked, true);
  assert.equal(quick('transcription').checked, true);
  assert.equal(p.calls.some(call => call.options.method === 'PUT' || call.path === '/api/start'), false);
});

test('session translation switch follows external settings and uses both directions when no previous selection exists', async t => {
  const p = await page(t, { language: 'en' });
  p.externalChange(config => { config.microphone.enabled = false; config.speaker.enabled = true; });
  await p.poll();
  assert.equal(p.byId('session-translation-enabled').checked, true);
  assert.equal(p.byId('session-translation-scope').textContent, 'Incoming audio only');
  p.set('session-translation-enabled', false);
  p.set('session-translation-enabled', true);
  assert.equal(p.byId('microphone-enabled').checked, false);
  assert.equal(p.byId('speaker-enabled').checked, true);
  p.byId('reload-config').click();
  await settle(() => p.byId('save').disabled && !p.byId('session-translation-enabled').disabled);
  p.externalChange(config => { config.microphone.enabled = false; config.speaker.enabled = false; config.recording.enabled = true; });
  await p.poll();
  assert.equal(p.byId('session-translation-enabled').checked, false);
  assert.equal(p.byId('session-recording-enabled').checked, true);
  p.set('session-translation-enabled', true);
  assert.equal(p.byId('microphone-enabled').checked, true);
  assert.equal(p.byId('speaker-enabled').checked, true);
  assert.equal(p.byId('session-translation-scope').textContent, 'Both directions');
});

test('session switches save independent recording or transcription choices before starting with the new revision', async t => {
  for (const feature of ['recording', 'transcription']) {
    const p = await page(t, { language: 'en' });
    const other = feature === 'recording' ? 'transcription' : 'recording';
    p.set('session-translation-enabled', false);
    p.set('session-' + feature + '-enabled', true);
    p.set(feature + '-microphone', false);
    p.set('session-' + feature + '-enabled', false);
    p.set('session-' + feature + '-enabled', true);
    assert.equal(p.byId(feature + '-enabled').checked, true);
    assert.equal(p.byId(feature + '-microphone').checked, false, 'quick switches retain the selected sources');
    assert.equal(p.byId(feature + '-speaker').checked, true);
    assert.equal(p.byId(other + '-enabled').checked, false);
    assert.equal(p.calls.some(call => call.options.method === 'PUT' || call.path === '/api/start'), false);
    p.byId('start').click();
    await settle(() => !p.byId('stop').hidden);
    const writes = p.calls.filter(call => call.options.method === 'PUT' || call.path === '/api/start');
    assert.deepEqual(writes.map(call => call.path), ['/api/config', '/api/start']);
    assert.equal(writes[0].options.headers['If-Match'], '"0"');
    assert.equal(writes[1].options.headers['If-Match'], '"1"');
    assert.equal(p.config().microphone.enabled, false);
    assert.equal(p.config().speaker.enabled, false);
    assert.equal(p.config()[feature].enabled, true);
    assert.equal(p.config()[other].enabled, false);
    assert.equal(p.config()[feature].microphone, false);
    assert.equal(writes[1].body.history_seconds, 0, 'quick feature selection never opts into history');
  }
});

test('session switches prevent an empty session and update history eligibility without selecting history', async t => {
  const p = await page(t, { language: 'en' });
  p.set('session-translation-enabled', false);
  assert.equal(p.byId('start').disabled, true);
  p.byId('start').click();
  assert.equal(p.calls.some(call => call.path === '/api/start'), false);
  for (const feature of ['recording', 'transcription']) {
    p.set(`session-${feature}-enabled`, true);
    assert.equal(p.byId('start').disabled, false);
    assert.equal(p.byId('history-include').disabled, false);
    assert.equal(p.byId('history-include').checked, false);
    p.set(`session-${feature}-enabled`, false);
    assert.equal(p.byId('start').disabled, true);
    assert.equal(p.byId('history-include').disabled, true);
    assert.equal(p.byId('history-include').checked, false);
  }
});

test('session switches are locked while starting, running or resolving a configuration conflict', async t => {
  let releaseStart;
  const pendingStart = new Promise(resolve => { releaseStart = resolve; });
  t.after(() => releaseStart());
  const p = await page(t, { startRequest: () => pendingStart });
  const switches = ['translation', 'recording', 'transcription'].map(feature => p.byId(`session-${feature}-enabled`));
  p.byId('start').click();
  await settle(() => p.calls.some(call => call.path === '/api/start'));
  assert.equal(p.byId('cancel-start').hidden, false);
  for (const control of switches) assert.equal(control.disabled, true, 'starting locks feature selection');
  releaseStart();
  await settle(() => !p.byId('stop').hidden);
  for (const control of switches) assert.equal(control.disabled, true, 'running locks feature selection');
  p.byId('stop').click();
  await settle(() => p.byId('stop').hidden && switches.every(control => !control.disabled));
  p.set('session-recording-enabled', true);
  p.externalChange(config => { config.speaker.target_language = 'fr-FR'; });
  await p.poll();
  assert.equal(p.byId('config-conflict').hidden, false);
  for (const control of switches) assert.equal(control.disabled, true, 'a conflict requires reloading before editing quick choices');
  assert.equal(p.byId('start').disabled, true);
  p.byId('reload-config').click();
  await settle(() => p.byId('config-conflict').hidden && switches.every(control => !control.disabled));
  assert.equal(p.byId('session-recording-enabled').checked, false, 'reloading discards the conflicting draft');
});

test('each route has its own provider; dedicated modes remove unsupported settings and retain every profile', async t => {
  const p = await page(t);
  assert.equal(p.window.location.hash, '');
  assert.equal(p.byId('microphone-source_language').value, 'Automático');
  p.set('speaker-provider', 'openai');
  p.set('profile-openai-model', 'gpt-realtime-2.1');
  p.set('speaker-prompt', 'Use termos técnicos.');
  assert.equal(p.byId('speaker-source_language').value, 'en-US');
  assert.equal(p.byId('speaker-prompt').disabled, false);
  assert.equal(p.byId('microphone-prompt').disabled, true);
  p.byId('save').click();
  await settle(() => p.byId('notice').textContent === 'Ajustes salvos.');
  assert.equal(p.config().microphone.provider, 'gemini');
  assert.equal(p.config().speaker.provider, 'openai');
  assert.equal(p.config().speaker.prompt, 'Use termos técnicos.');
  assert.deepEqual(Object.keys(p.config().providers).sort(), ['gemini', 'local', 'openai']);
  p.set('profile-openai-model', 'gpt-realtime-translate-2026-09-29');
  p.byId('save').click();
  await settle(() => p.config().speaker.prompt === '');
  assert.equal(p.config().speaker.source_language, 'en-US');
  assert.equal(Object.hasOwn(p.config().speaker, 'voice'), false);
});

test('temporary keys use a separate authenticated endpoint, clear the input, and never enter configuration', async t => {
  const p = await page(t);
  p.byId('credential-gemini').value = 'fake-test-only-key';
  p.doc.querySelector('.credential-apply[data-provider="gemini"]').click();
  await settle(() => p.byId('notice').textContent.includes('Chave aplicada'));
  assert.equal(p.byId('credential-gemini').value, '');
  assert.equal(p.calls.find(call => call.path === '/api/credentials' && call.options.method === 'POST').body.key, 'fake-test-only-key');
  assert.equal(JSON.stringify(p.config()).includes('fake-test-only-key'), false);
  p.doc.querySelector('.credential-clear[data-provider="gemini"]').click();
  await settle(() => p.byId('notice').textContent.includes('Chave temporária removida'));
  assert.match(p.byId('credential-gemini-status').textContent, /Nenhuma chave/);
});

test('translation uses only native default voices without library, uploads or synthesis profiles', async t => {
  const p = await page(t, { language: 'en' });
  assert.equal(p.byId('nav-translation').textContent, 'Translation');
  assert.equal(p.doc.querySelectorAll('.native-voice-hint').length, 2);
  assert.equal(p.doc.querySelector('[data-voice-route], .library-open, input[type="file"]'), null);
  for (const id of ['voice-library', 'profile-elevenlabs', 'profile-gemini-voice', 'profile-openai-voice', 'profile-gemini-tts_model', 'profile-local-piper_voice']) assert.equal(p.byId(id), null);
  assert.deepEqual([...p.byId('profile-selector').options].map(option => option.value), ['gemini', 'openai', 'local']);
  for (const provider of ['local', 'openai', 'gemini']) {
    for (const route of ['microphone', 'speaker']) p.set(`${route}-provider`, provider);
    p.byId('save').click();
    await settle(() => p.config().microphone.provider === provider && !p.byId('settings').disabled);
    for (const route of ['microphone', 'speaker']) assert.equal(Object.hasOwn(p.config()[route], 'voice'), false);
    for (const cloud of ['gemini', 'openai']) for (const field of ['voice', 'tts_model']) assert.equal(Object.hasOwn(p.config().providers[cloud], field), false);
    assert.equal(Object.hasOwn(p.config().providers, 'elevenlabs'), false);
    assert.equal(Object.hasOwn(p.config().providers.local, 'piper_voice'), false);
  }
  assert.equal(p.calls.some(call => call.path.startsWith('/api/voices')), false);
  assert.equal(p.calls.some(call => call.path === '/api/start'), false);
});

test('a local route keeps its native Piper endpoint while preserving other translation profiles', async t => {
  const p = await page(t);
  p.set('microphone-provider', 'local');
  p.set('profile-local-piper_endpoint-mode', 'external');
  p.set('profile-local-piper_endpoint', 'http://127.0.0.1:5001/synthesize');
  p.byId('save').click();
  await settle(() => p.byId('notice').textContent === 'Ajustes salvos.');
  assert.equal(p.config().providers.local.piper_endpoint, 'http://127.0.0.1:5001/synthesize');
  assert.equal(p.config().speaker.provider, 'gemini');
  assert.equal(p.config().providers.openai.model, 'gpt-realtime-translate');
  assert.equal(Object.hasOwn(p.config().microphone, 'voice'), false);
});

test('starting locks configuration and stopping unlocks it; device refresh retains an empty selection', async t => {
  const p = await page(t);
  p.set('microphone-capture_device', '');
  p.byId('refresh-devices').click();
  await settle(() => p.byId('notice').textContent.includes('Lista de dispositivos'));
  assert.equal(p.byId('microphone-capture_device').value, '');
  p.set('microphone-capture_device', 'physical-mic');
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.byId('settings').disabled, true);
  assert.equal(p.byId('session-name').disabled, true);
  assert.deepEqual(p.calls.find(call => call.path === '/api/start').body, { history_seconds: 0 });
  assert.equal(p.byId('session-name').value, '', 'an automatic session must not fill the next session name');
  p.byId('stop').click();
  await settle(() => p.byId('stop').hidden);
  assert.equal(p.byId('settings').disabled, false);
  assert.equal(p.byId('session-name').disabled, false);
});

test('a session name belongs only to start, is escaped in status, and remains visible after stopping', async t => {
  const p = await page(t);
  const name = '<img src=x onerror=alert(1)> Reunião';
  p.set('session-name', name);
  assert.equal(p.byId('save').disabled, true, 'a session name is not a configuration edit');
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.calls.find(call => call.path === '/api/start').body.name, name);
  assert.equal(p.byId('session-identity').textContent, `Sessão: ${name}`);
  assert.equal(p.byId('session-identity').querySelectorAll('img').length, 0);
  assert.equal(JSON.stringify(p.config()).includes(name), false);
  p.byId('stop').click();
  await settle(() => p.byId('stop').hidden);
  assert.equal(p.byId('session-identity').textContent, `Última sessão: ${name}`);
});

test('autostart is read on load and changes only after its explicit apply action', async t => {
  const p = await page(t);
  await settle(() => !p.byId('autostart-enabled').disabled);
  const writes = () => p.calls.filter(call => call.path === '/api/autostart' && call.options.method === 'POST');
  assert.equal(writes().length, 0);
  p.byId('autostart-enabled').click();
  assert.equal(p.byId('autostart-apply').disabled, false);
  assert.equal(writes().length, 0, 'toggling prepares the preference without changing the system');
  p.set('microphone-target_language', 'fr-FR');
  p.byId('save').click();
  await settle(() => p.byId('notice').textContent === 'Ajustes salvos.');
  assert.equal(writes().length, 0, 'saving audio configuration must not enable autostart');
  p.byId('autostart-apply').click();
  await settle(() => p.byId('notice').textContent.includes('Início automático ativado'));
  assert.deepEqual(writes().map(call => call.body), [{ enabled: true }]);
  assert.equal(p.byId('autostart-apply').disabled, true);
  assert.equal(p.calls.some(call => call.path === '/api/start'), false);
});

test('systemd startup method and exact entry path localize without applying a pending preference', async t => {
  const entry = '/home/test/.config/systemd/user/org.babel.audio.service.d/<custom>.conf';
  const p = await page(t, { autostartMetadata: { method: 'systemd_user', entry_path: entry } });
  await settle(() => p.byId('autostart-status').textContent.includes('systemd'));
  assert.ok(p.byId('autostart-status').textContent.includes('serviço systemd do usuário'));
  assert.ok(p.byId('autostart-status').textContent.includes(entry));
  assert.equal(p.byId('autostart-status').querySelector('custom'), null);
  p.byId('autostart-enabled').click();
  p.byId('interface-language').value = 'en';
  p.byId('interface-language').dispatchEvent(new p.window.Event('change', { bubbles: true }));
  await settle(() => p.byId('autostart-status').textContent.includes('Startup method: systemd user service.'));
  assert.equal(p.byId('autostart-enabled').checked, true);
  assert.equal(p.calls.some(call => call.path === '/api/autostart' && call.options.method === 'POST'), false);
  assert.equal(p.calls.some(call => call.path === '/api/start'), false);
});

test('external configuration revisions refresh clean settings once and protect unsaved edits', async t => {
  const p = await page(t);
  const reads = () => p.calls.filter(call => call.path === '/api/config' && call.options.method === 'GET').length;
  const initialReads = reads();
  await p.poll();
  assert.equal(reads(), initialReads, 'status polling must not repeatedly fetch unchanged configuration');
  p.externalChange(config => { config.microphone.capture_device = 'new-tray-mic'; });
  await p.poll();
  assert.equal(p.byId('microphone-capture_device').value, 'new-tray-mic');
  assert.equal(reads(), initialReads + 1);
  await p.poll();
  assert.equal(reads(), initialReads + 1);
  p.set('speaker-target_language', 'ja-JP');
  p.externalChange(config => { config.speaker.playback_device = 'new-tray-speaker'; });
  await p.poll();
  assert.equal(p.byId('speaker-target_language').value, 'ja-JP', 'a draft must remain visible');
  assert.equal(p.byId('config-conflict').hidden, false);
  assert.equal(p.byId('save').disabled, true);
  assert.equal(p.byId('start').disabled, true);
  assert.equal(reads(), initialReads + 1, 'dirty settings require explicit reload');
  p.byId('reload-config').click();
  await settle(() => p.byId('config-conflict').hidden && !p.byId('start').disabled);
  assert.equal(p.byId('speaker-target_language').value, 'pt-BR');
  assert.equal(p.byId('speaker-playback_device').value, 'new-tray-speaker');
  assert.equal(p.byId('save').disabled, true);
});

test('If-Match prevents stale save and stale start even before the next status poll', async t => {
  const p = await page(t);
  p.set('microphone-target_language', 'fr-FR');
  p.externalChange(config => { config.microphone.capture_device = 'changed-before-save'; });
  p.byId('save').click();
  await settle(() => !p.byId('config-conflict').hidden);
  assert.equal(p.config().microphone.target_language, 'en-US');
  assert.equal(p.config().microphone.capture_device, 'changed-before-save');
  assert.equal(p.calls.find(call => call.options.method === 'PUT').options.headers['If-Match'], '"0"');
  await settle(() => !p.byId('reload-config').disabled);
  p.byId('reload-config').click();
  await settle(() => !p.byId('start').disabled);
  p.externalChange(config => { config.speaker.playback_device = 'changed-before-start'; });
  p.byId('start').click();
  await settle(() => !p.byId('config-conflict').hidden);
  assert.equal(p.byId('stop').hidden, true);
  assert.equal(p.calls.find(call => call.path === '/api/start').options.headers['If-Match'], '"1"');
});

test('the first status reconciles tray changes between the atomic configuration snapshot and initialization', async t => {
  const p = await page(t, { initialChange: config => { config.microphone.capture_device = 'changed-during-initialization'; } });
  assert.equal(p.byId('microphone-capture_device').value, 'changed-during-initialization');
  p.set('microphone-target_language', 'de-DE');
  p.byId('save').click();
  await settle(() => p.byId('notice').textContent === 'Ajustes salvos.');
  assert.equal(p.calls.find(call => call.options.method === 'PUT').options.headers['If-Match'], '"1"');
  p.byId('start').click();
  await settle(() => p.byId('stop').hidden === false);
  assert.equal(p.calls.find(call => call.path === '/api/start').options.headers['If-Match'], '"2"');
});

test('recording and original transcription save independently with one shared filename pattern', async t => {
  const p = await page(t);
  assert.equal(p.byId('recording-enabled').checked, false);
  assert.equal(p.byId('recording-directory').disabled, false);
  p.set('recording-enabled', true);
  p.set('recording-directory', 'audio-original');
  p.set('recording-speaker', false);
  p.set('files-name_pattern', '{session}-{id}');
  p.byId('save').click();
  await settle(() => p.byId('notice').textContent === 'Ajustes salvos.');
  assert.deepEqual(p.config().recording, { ...defaults().recording, enabled: true, microphone: true, speaker: false, directory: 'audio-original' });
  assert.equal(p.config().transcription.enabled, false);
  assert.equal(p.config().files.name_pattern, '{session}-{id}');
  p.set('transcription-enabled', true);
  p.set('transcription-microphone', false);
  p.set('transcription-directory', 'texto-original');
  p.byId('save').click();
  await settle(() => p.config().transcription.enabled);
  assert.deepEqual(p.config().transcription, { ...defaults().transcription, enabled: true, microphone: false, speaker: true, timestamps: true, directory: 'texto-original' });
  assert.equal(p.config().recording.microphone, true);
  assert.equal(p.config().recording.speaker, false);
  assert.equal(p.doc.querySelector('.transcription-settings').textContent.includes('um único .txt'), true);
  assert.equal(p.doc.querySelector('.recording-settings').textContent.includes('único WAV'), true);
});

test('recording balance round-trips independently of translation, recognition, history and routing on every host', async t => {
  for (const platform of ['linux', 'macos', 'windows']) {
    const p = await page(t, { platform, language: 'en' });
    const before = structuredClone(p.config());
    p.set('recording-enabled', true);
    p.set('recording-microphone-gain', 12.25);
    p.set('recording-speaker-gain', -3.125);
    p.set('recording-ducking', 16.125);
    p.set('recording-microphone-threshold', -55.5);
    p.byId('save').click();
    await settle(() => p.config().recording.mix.microphone_gain_db === 12.25 && !p.byId('settings').disabled);
    assert.deepEqual(p.config().recording.mix, { microphone_gain_db: 12.25, speaker_gain_db: -3.125, microphone_priority: true, ducking_db: 16.125, microphone_threshold_db: -55.5 });
    for (const section of ['microphone', 'speaker', 'audio', 'history', 'transcription']) assert.deepEqual(p.config()[section], before[section], `${platform}: recording mix must not change ${section}`);
    const priority = p.byId('recording-microphone-priority');
    assert.ok(priority.closest('label.switch').querySelector('.switch-track'));
    assert.equal(p.byId('recording-priority-label').textContent, 'Keep microphone audible');
    assert.equal(p.doc.querySelector('.recording-mix-advanced').open, false);
    p.byId('start').click();
    await settle(() => !p.byId('stop').hidden);
    for (const control of p.doc.querySelectorAll('[data-recording-mix]')) assert.equal(control.matches(':disabled'), true, `${platform}: active session balance stays fixed`);
    p.byId('stop').click();
    await settle(() => p.byId('stop').hidden && !p.byId('settings').disabled);
    assert.equal(p.byId('recording-microphone-gain').value, '12.25');
    assert.equal(p.byId('recording-speaker-gain').value, '-3.125');
  }
});

test('recording balance disables excluded sources and inactive priority without losing their saved levels', async t => {
  const p = await page(t);
  const fields = [...p.doc.querySelectorAll('[data-recording-mix]')];
  assert.equal(fields.length, 5);
  for (const control of fields) assert.equal(control.disabled, true);
  assert.equal(p.byId('recording-priority-label').textContent, 'Manter o microfone audível');
  p.set('recording-enabled', true);
  for (const control of fields) assert.equal(control.disabled, false);
  p.set('recording-microphone-gain', 12);
  p.set('recording-speaker-gain', -6);
  p.set('recording-microphone', false);
  assert.equal(p.byId('recording-microphone-gain').disabled, true);
  assert.equal(p.byId('recording-speaker-gain').disabled, false);
  for (const id of ['recording-microphone-priority', 'recording-ducking', 'recording-microphone-threshold']) assert.equal(p.byId(id).disabled, true);
  p.set('recording-microphone', true);
  p.set('recording-speaker', false);
  assert.equal(p.byId('recording-microphone-gain').disabled, false);
  assert.equal(p.byId('recording-speaker-gain').disabled, true);
  p.set('recording-speaker', true);
  p.set('recording-microphone-priority', false);
  assert.equal(p.byId('recording-microphone-priority').disabled, false);
  for (const id of ['recording-ducking', 'recording-microphone-threshold']) assert.equal(p.byId(id).disabled, true);
  p.byId('save').click();
  await settle(() => p.config().recording.mix.microphone_priority === false && !p.byId('settings').disabled);
  assert.deepEqual(p.config().recording.mix, { microphone_gain_db: 12, speaker_gain_db: -6, microphone_priority: false, ducking_db: 12, microphone_threshold_db: -50 });
  p.set('recording-microphone-gain', 25);
  assert.equal(p.byId('recording-microphone-gain').checkValidity(), false);
  p.set('recording-microphone-gain', -24);
  assert.equal(p.byId('recording-microphone-gain').checkValidity(), true);
  p.set('recording-microphone-priority', true);
  p.set('recording-ducking', 31);
  assert.equal(p.byId('recording-ducking').checkValidity(), false);
  p.set('recording-ducking', 30);
  assert.equal(p.byId('recording-ducking').checkValidity(), true);
  p.set('recording-microphone-threshold', -61);
  assert.equal(p.byId('recording-microphone-threshold').checkValidity(), false);
  p.set('recording-microphone-threshold', -60);
  assert.equal(p.byId('recording-microphone-threshold').checkValidity(), true);
});

test('older recording settings receive complete balance defaults before editing and saving', async t => {
  const p = await page(t, { initialChange: config => { delete config.recording.mix; } });
  assert.equal(p.byId('recording-microphone-gain').value, '0');
  assert.equal(p.byId('recording-speaker-gain').value, '0');
  assert.equal(p.byId('recording-microphone-priority').checked, true);
  assert.equal(p.byId('recording-ducking').value, '12');
  assert.equal(p.byId('recording-microphone-threshold').value, '-50');
  p.set('recording-enabled', true);
  p.set('recording-microphone-gain', 6);
  p.byId('save').click();
  await settle(() => p.config().recording.mix?.microphone_gain_db === 6);
  assert.deepEqual(p.config().recording.mix, { ...defaults().recording.mix, microphone_gain_db: 6 });
});

test('base and destination folders are editable and previewed with both file features off, and save for future sessions', async t => {
  const p = await page(t);
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  assert.equal(p.byId('files-base_path').value, '/home/test/Babel');
  assert.equal(p.byId('files-working-directory'), null);
  assert.equal(p.byId('files-resolved-base').textContent, '/home/test/Babel');
  assert.equal(p.byId('transcription-resolved-directory').textContent, '/home/test/Babel/transcripts');
  assert.equal(p.byId('recording-resolved-directory').textContent, '/home/test/Babel/recordings');
  assert.equal(p.byId('files-base_path').closest('[data-workspace-panel]'), p.byId('workspace-settings-files'));
  for (const id of ['files-base_path', 'transcription-directory', 'recording-directory']) assert.equal(p.byId(id).matches(':disabled'), false);
  assert.equal(p.byId('transcription-microphone').disabled, true);
  assert.equal(p.byId('recording-microphone').disabled, true);
  p.set('files-base_path', '/media/Session files');
  p.set('transcription-directory', 'texto');
  p.set('recording-directory', '/archive/original');
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  assert.equal(p.byId('transcription-resolved-directory').textContent, '/media/Session files/texto');
  assert.equal(p.byId('recording-resolved-directory').textContent, '/archive/original');
  assert.equal(p.calls.some(call => call.path === '/api/config' && call.options.method === 'PUT'), false);
  assert.equal(p.config().files.base_path, '/home/test/Babel', 'preview does not save a changed base');
  assert.equal(p.config().transcription.enabled, false);
  assert.equal(p.config().recording.enabled, false);
  p.byId('save').click();
  await settle(() => p.config().files.base_path === '/media/Session files');
  assert.equal(p.config().transcription.directory, 'texto');
  assert.equal(p.config().recording.directory, '/archive/original');
  assert.equal(p.config().transcription.enabled, false);
  assert.equal(p.config().recording.enabled, false);
  await settle(() => !p.byId('start').disabled);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  for (const id of ['files-base_path', 'transcription-directory', 'recording-directory']) assert.equal(p.byId(id).matches(':disabled'), true);
  assert.equal(p.byId('save').disabled, true);
  assert.equal(p.byId('transcription-resolved-directory').textContent, '/media/Session files/texto');
});

test('folder preview debounces edits and ignores both successful and failed stale replies', async t => {
  const p = await page(t);
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  const pending = [];
  p.filePaths((body, options, resolvePaths) => new Promise((resolve, reject) => pending.push({ body, options, resolve: () => resolve(resolvePaths(body)), reject })));
  const count = p.calls.filter(call => call.path === '/api/file-paths').length;
  p.set('files-base_path', '/draft'); p.set('files-base_path', '/draft-two'); p.set('files-base_path', '/first');
  assert.equal(p.byId('files-path-preview').getAttribute('aria-busy'), 'true');
  assert.equal(p.byId('files-resolved-base').textContent, '—', 'old path must not look current while typing');
  await settle(() => pending.length === 1);
  assert.equal(p.calls.filter(call => call.path === '/api/file-paths').length, count + 1);
  assert.equal(pending[0].body.base_path, '/first');
  p.set('files-base_path', '/second');
  assert.equal(pending[0].options.signal.aborted, true);
  await settle(() => pending.length === 2);
  pending[0].resolve(); await new Promise(resolve => setImmediate(resolve));
  assert.equal(p.byId('files-path-preview').dataset.state, 'loading');
  pending[1].resolve();
  await settle(() => p.byId('files-resolved-base').textContent === '/second');
  p.set('files-base_path', '/third');
  await settle(() => pending.length === 3);
  p.set('files-base_path', '/fourth');
  await settle(() => pending.length === 4);
  pending[3].resolve();
  await settle(() => p.byId('files-resolved-base').textContent === '/fourth');
  pending[2].reject(new Error('obsolete failure')); await new Promise(resolve => setImmediate(resolve));
  assert.equal(p.byId('files-path-preview').dataset.state, 'ready');
  assert.equal(p.byId('files-path-error').hidden, true);
  assert.equal(p.byId('files-path-preview').getAttribute('aria-busy'), 'false');
  assert.equal(p.calls.some(call => call.path === '/api/config' && call.options.method === 'PUT'), false);
});

test('folder preview errors and older-backend 404 stay inline, localize, clear stale paths and recover', async t => {
  const p = await page(t);
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  p.filePaths(() => new Response(JSON.stringify({error:'<img src=x onerror=alert(1)> caminho inválido'}), {status:400,headers:{'Content-Type':'application/json'}}));
  p.set('files-base_path', '/invalid');
  await settle(() => p.byId('files-path-preview').dataset.state === 'error');
  assert.equal(p.byId('error').hidden, true);
  assert.equal(p.byId('files-resolved-base').textContent, '—');
  assert.equal(p.byId('files-path-error').querySelector('img'), null);
  assert.match(p.byId('files-path-error').textContent, /Confira as pastas.*caminho inválido/);
  const calls = p.calls.filter(call => call.path === '/api/file-paths').length;
  await chooseInterface(p, 'en');
  assert.match(p.byId('files-path-error').textContent, /Check the folders.*caminho inválido/);
  assert.equal(p.calls.filter(call => call.path === '/api/file-paths').length, calls);
  p.filePaths(() => new Response('Not found', {status:404}));
  p.set('files-base_path', '/older');
  await settle(() => p.byId('files-path-preview').dataset.state === 'unsupported');
  assert.equal(p.byId('files-path-error').hidden, true);
  assert.match(p.byId('files-path-status').textContent, /version does not provide folder previews/);
  assert.equal(p.byId('files-base_path').value, '/older');
  assert.equal(p.byId('save').disabled, false, 'preview failure must not freeze configuration editing');
  p.filePaths(null); p.set('files-base_path', '/recovered');
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  assert.equal(p.byId('files-resolved-base').textContent, '/recovered');
  assert.equal(p.byId('error').hidden, true);
});

test('folder preview preserves Windows host paths across browser OS and locale changes without resolving locally', async t => {
  const hostPaths = { base_path: 'D:\\Sessões', transcription_directory: 'D:\\Sessões\\texto', recording_directory: '\\\\nas\\audio\\arquivo' };
  const p = await page(t, { platform:'windows', userAgent:'Mozilla/5.0 (X11; Linux x86_64)', filePaths: () => hostPaths });
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  assert.equal(p.byId('files-working-directory'), null);
  assert.equal(p.byId('files-resolved-base').textContent, hostPaths.base_path);
  assert.equal(p.byId('transcription-resolved-directory').textContent, hostPaths.transcription_directory);
  assert.equal(p.byId('recording-resolved-directory').textContent, hostPaths.recording_directory);
  await chooseInterface(p, 'en');
  assert.match(p.byId('files-path-status').textContent, /Full paths on the computer running Babel/);
  assert.equal(p.byId('recording-resolved-directory').textContent, hostPaths.recording_directory);
  p.window.BabelI18n.apply();
  assert.equal(p.byId('files-path-status').textContent.startsWith('Full paths'), true);
  assert.match(p.byId('files-base-hint').textContent, /environment variables are not expanded/);
});

test('malformed folder responses remain inline and never render invented destination paths', async t => {
  const p = await page(t, {filePaths: () => ({base_path:'/partial'})});
  await settle(() => p.byId('files-path-preview').dataset.state === 'error');
  assert.match(p.byId('files-path-error').textContent, /prévia de pastas incompleta/);
  assert.equal(p.byId('recording-resolved-directory').textContent, '—');
  assert.equal(p.byId('error').hidden, true);
});

test('Linux and macOS bases must be absolute and cannot fall back to the launch folder or save invalid values', async t => {
  for (const [platform, base] of [['linux', '/home/ana/Babel'], ['macos', '/Users/ana/Babel']]) {
    const p = await page(t, {platform, basePath:base, userAgent:'Mozilla/5.0 (Windows NT 10.0; Win64; x64)'});
    await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
    assert.equal(p.byId('files-base_path').value, base);
    assert.equal(p.byId('files-resolved-base').textContent, base);
    const previews = () => p.calls.filter(call => call.path === '/api/file-paths').length;
    const count = previews();
    for (const value of ['', '.', './recordings', '../recordings', 'recordings', '~', '~/Babel', '$HOME/Babel', 'C:\\Babel']) {
      p.set('files-base_path', value);
      assert.equal(p.byId('files-base_path').validity.valid, false, value);
      assert.equal(p.byId('files-path-preview').dataset.state, 'invalid', value);
      assert.equal(p.byId('files-resolved-base').textContent, '—');
      p.byId('save').click();
      assert.equal(p.calls.some(call => call.path === '/api/config' && call.options.method === 'PUT'), false);
    }
    await new Promise(resolve => setTimeout(resolve, 350));
    assert.equal(previews(), count);
    assert.equal(p.window.BabelWorkspace.current, 'settings');
    assert.match(p.byId('files-base-error').textContent, /pasta base absoluta começando com \//);
    assert.equal(p.byId('files-base_path').placeholder, '/pasta/absoluta');
    await chooseInterface(p, 'en');
    assert.match(p.byId('files-base_path').validationMessage, /absolute base folder starting with \//);
    p.set('files-base_path', `${base}/My sessions`);
    await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
    assert.equal(p.byId('files-base-error').hidden, true);
    assert.equal(p.byId('files-base_path').validity.valid, true);
    p.byId('save').click();
    await settle(() => p.config().files.base_path === `${base}/My sessions`);
  }
  const missing = await page(t, {missingBase:true});
  assert.equal(missing.byId('files-base_path').value, '');
  assert.equal(missing.byId('files-base_path').validity.valid, false);
  assert.equal(missing.calls.some(call => call.path === '/api/file-paths'), false);
});

test('Windows absolute drive and UNC bases use host syntax, rejecting drive-relative and incomplete UNC values', async t => {
  const p = await page(t, {platform:'windows', userAgent:'Mozilla/5.0 (X11; Linux x86_64)'});
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  assert.equal(p.byId('files-base_path').value, 'C:\\Users\\Test\\Babel');
  assert.equal(p.byId('files-base_path').placeholder, 'C:\\Babel');
  const before = p.calls.filter(call => call.path === '/api/file-paths').length;
  for (const value of ['.', 'sessions', '~', 'C:', 'C:sessions', '\\sessions', '/sessions', '\\\\server', '\\\\server\\', '\\\\?\\C:sessions', '\\\\?\\UNC\\server']) {
    p.set('files-base_path', value);
    assert.equal(p.byId('files-base_path').validity.valid, false, value);
    p.byId('save').click();
    assert.equal(p.calls.some(call => call.path === '/api/config' && call.options.method === 'PUT'), false);
  }
  await new Promise(resolve => setTimeout(resolve, 350));
  assert.equal(p.calls.filter(call => call.path === '/api/file-paths').length, before);
  assert.match(p.byId('files-base-error').textContent, /unidade e raiz.*UNC completo/);
  for (const value of ['D:\\Babel', 'E:/Babel', '\\\\server\\share', '\\\\server\\share\\Sessões', '//server/share/Babel', '\\\\?\\C:\\Babel', '\\\\?\\UNC\\server\\share\\Babel']) {
    p.set('files-base_path', value);
    assert.equal(p.byId('files-base_path').validity.valid, true, value);
  }
  p.set('files-base_path', '\\\\nas\\calls\\Babel');
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  assert.equal(p.byId('files-resolved-base').textContent, '\\\\nas\\calls\\Babel');
  p.byId('save').click();
  await settle(() => p.config().files.base_path === '\\\\nas\\calls\\Babel');
});

test('an invalid base cancels pending previews and unavailable host metadata cannot infer the browser path format', async t => {
  const p = await page(t);
  await settle(() => p.byId('files-path-preview').dataset.state === 'ready');
  let complete; let requestSignal;
  p.filePaths((body, options, resolvePaths) => new Promise(resolve => {requestSignal = options.signal; complete = () => resolve(resolvePaths(body));}));
  p.set('files-base_path', '/pending');
  await settle(() => complete !== undefined);
  p.set('files-base_path', '.');
  assert.equal(requestSignal.aborted, true);
  complete(); await new Promise(resolve => setImmediate(resolve));
  assert.equal(p.byId('files-path-preview').dataset.state, 'invalid');
  assert.equal(p.byId('files-resolved-base').textContent, '—');
  const unknown = await page(t, {platform:'windows', platformFailure:true, userAgent:'Mozilla/5.0 (X11; Linux x86_64)'});
  assert.equal(unknown.byId('files-base_path').validity.valid, true);
  assert.match(unknown.byId('files-base-error').textContent, /sistema operacional.*indisponível/);
  assert.equal(unknown.byId('files-base-error').getAttribute('role'), 'status');
  assert.equal(unknown.byId('files-path-preview').dataset.state, 'unavailable');
  assert.equal(unknown.calls.some(call => call.path === '/api/file-paths'), false);
  unknown.byId('start').click();
  await settle(() => !unknown.byId('stop').hidden && !unknown.byId('stop').disabled);
  assert.equal(unknown.calls.some(call => call.path === '/api/start'), true, 'platform metadata failure cannot prevent using the saved absolute configuration');
  unknown.byId('stop').click();
  await settle(() => unknown.byId('stop').hidden && !unknown.byId('settings').disabled);
  unknown.set('files-base_path', 'D:\\Saved sessions');
  unknown.byId('save').click();
  await settle(() => unknown.config().files.base_path === 'D:\\Saved sessions' && !unknown.byId('refresh-devices').disabled);
  assert.equal(unknown.calls.some(call => call.path === '/api/file-paths'), false, 'only previews wait for platform recovery');
  unknown.set('files-base_path', '.');
  assert.equal(unknown.byId('files-base_path').validity.valid, false, 'plain relative syntax remains invalid without assuming a host OS');
  unknown.set('files-base_path', 'D:\\Saved sessions');
  unknown.platformFailure(false); unknown.byId('refresh-devices').click();
  await settle(() => unknown.byId('files-path-preview').dataset.state === 'ready');
  assert.equal(unknown.byId('files-base_path').validity.valid, true);
  assert.equal(unknown.byId('files-base_path').placeholder, 'C:\\Babel');
});

test('physical device changes from the tray refresh the displayed routes during an active session', async t => {
  const p = await page(t);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
  p.externalChange(config => { config.microphone.capture_device = 'hot-swapped-microphone'; config.speaker.playback_device = 'hot-swapped-headphones'; });
  await p.poll();
  assert.equal(p.byId('microphone-capture_device').value, 'hot-swapped-microphone');
  assert.equal(p.byId('speaker-playback_device').value, 'hot-swapped-headphones');
  assert.equal(p.byId('settings').disabled, true);
  assert.equal(p.byId('stop').hidden, false);
  assert.equal(p.byId('config-conflict').hidden, true);
});

test('idle routes pass original audio; recording-only sessions do not require translation', async t => {
  const p = await page(t);
  assert.equal(p.byId('session-state').textContent, 'Áudio original');
  assert.equal(p.byId('microphone-signal').textContent, 'Original');
  assert.equal(p.byId('microphone-output-label').textContent, 'Original');
  assert.equal(p.byId('start').textContent.includes('Iniciar sessão'), true);
  p.set('microphone-enabled', false);
  p.set('speaker-enabled', false);
  assert.equal(p.byId('start').disabled, true);
  assert.equal(p.byId('session-hint').textContent.includes('sem sessão'), true);
  p.set('recording-enabled', true);
  assert.equal(p.byId('start').disabled, false);
  assert.equal(p.byId('recording-microphone').disabled, false);
  assert.equal(p.byId('recording-speaker').disabled, false);
  assert.equal(p.byId('microphone-capture_device').disabled, false);
  assert.equal(p.byId('microphone-target_language').disabled, true);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
  assert.equal(p.config().microphone.enabled, false);
  assert.equal(p.config().speaker.enabled, false);
  assert.equal(p.config().recording.enabled, true);
  assert.equal(p.byId('session-state').textContent, 'Sessão ativa');
  assert.equal(p.byId('microphone-state').textContent, 'Áudio original');
  assert.equal(p.byId('microphone-signal').textContent, 'Original');
  p.byId('stop').click();
  await settle(() => p.byId('stop').hidden);
  assert.equal(p.byId('session-state').textContent, 'Áudio original');
  assert.equal(p.byId('speaker-signal').textContent, 'Original');
});

test('transcription sources and ASR models remain selectable with both translations disabled', async t => {
  const p = await page(t);
  p.set('microphone-enabled', false);
  p.set('speaker-enabled', false);
  p.set('transcription-enabled', true);
  p.set('transcription-microphone', true);
  p.set('transcription-speaker', true);
  p.set('stt-profile-gemini-model', 'fixture-asr-model');
  assert.equal(p.byId('transcription-microphone').disabled, false);
  assert.equal(p.byId('transcription-speaker').disabled, false);
  assert.equal(p.byId('microphone-source_language').disabled, false);
  assert.equal(p.byId('speaker-source_language').disabled, false);
  assert.equal(p.byId('profile-gemini-transcription_model').closest('label').hidden, true);
  assert.equal(p.byId('footer-state').textContent, 'Sessão configurada com provedores de nuvem');
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.config().recording.enabled, false);
  assert.equal(p.config().transcription.enabled, true);
  assert.equal(p.config().transcription.providers.gemini.model, 'fixture-asr-model');
  assert.equal(p.config().providers.gemini.transcription_model, '');
  assert.equal(p.byId('speaker-output-label').textContent, 'Original');
});

test('routing failures are visible even without a running processing session and escape remote text', async t => {
  const p = await page(t);
  p.routing(false, '<img src=x onerror=alert(1)> dispositivo desconectado');
  await p.poll();
  assert.equal(p.byId('session-state').textContent, 'Sem roteamento');
  assert.equal(p.byId('routing-error').hidden, false);
  assert.equal(p.byId('routing-error').querySelectorAll('img').length, 0);
  assert.equal(p.byId('routing-error').textContent.includes('dispositivo desconectado'), true);
  assert.equal(p.byId('microphone-signal').textContent, '—');
  p.routing(true);
  await p.poll();
  assert.equal(p.byId('routing-error').hidden, true);
  assert.equal(p.byId('session-state').textContent, 'Áudio original');
});

test('a waiting virtual endpoint has no active diagram or stale levels while the other route keeps working', async t => {
  const p = await page(t);
  p.routeStatus('microphone', { input_level: 0.5, output_level: 0.25, captured_frames: 123, last_input_transcript: 'Original microphone words' });
  p.routeStatus('speaker', { input_level: 0.75, output_level: 0.5 });
  await p.poll();
  assert.equal(p.byId('microphone-input').value, 0.5);
  p.routeStatus('microphone', { state: 'waiting_for_app', input_level: 0.5, output_level: 0.25, captured_frames: 123, last_input_transcript: 'Original microphone words' });
  await p.poll();
  assert.equal(p.byId('session-state').textContent, 'Áudio original');
  assert.equal(p.byId('microphone-state').textContent, 'Roteamento inativo');
  assert.match(p.byId('microphone-state').title, /microfone virtual/);
  assert.equal(p.byId('microphone-signal').textContent, '—');
  assert.equal(p.byId('microphone-signal').classList.contains('original'), false);
  assert.match(p.byId('microphone-signal').parentElement.getAttribute('aria-label'), /microfone virtual ser o padrão do sistema ou ser usado por um aplicativo/);
  for (const side of ['input', 'output']) {
    assert.equal(p.byId(`microphone-${side}`).value, 0);
    assert.equal(p.byId(`microphone-${side}-db`).textContent, '−∞ dB');
  }
  assert.equal(p.byId('microphone-captured_frames').textContent, '123');
  assert.equal(p.byId('microphone-input-transcript').textContent, 'Original microphone words');
  assert.equal(p.byId('speaker-state').textContent, 'Áudio original');
  assert.equal(p.byId('speaker-signal').textContent, 'Original');
  assert.equal(p.byId('speaker-input').value, 0.75);
  assert.equal(p.calls.some(call => call.options.method !== 'GET'), false, 'status changes must not mutate settings or sessions');
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
  assert.equal(p.byId('session-state').textContent, 'Sessão ativa');
  assert.equal(p.byId('microphone-state').textContent, 'Roteamento inativo');
  assert.equal(p.byId('microphone-signal').textContent, '—');
  assert.equal(p.byId('speaker-signal').textContent, 'IA');
  p.routeStatus('microphone', { input_level: 0.3, output_level: 0.2 });
  await p.poll();
  assert.equal(p.byId('microphone-state').textContent, 'Transmitindo');
  assert.equal(p.byId('microphone-state').title, '');
  assert.equal(p.byId('microphone-signal').textContent, 'IA');
  assert.equal(p.byId('microphone-input').value, 0.3);
});

test('both endpoints waiting is distinct from no routing and does not stop a recording-only session', async t => {
  const p = await page(t);
  p.routing(false);
  for (const route of ['microphone', 'speaker']) p.routeStatus(route, { state: 'waiting_for_app', input_level: 0.8, output_level: 0.7 });
  await p.poll();
  assert.equal(p.byId('session-state').textContent, 'Roteamento inativo');
  assert.equal(p.byId('status-dot').classList.contains('running'), false);
  assert.equal(p.byId('routing-error').hidden, true);
  p.set('microphone-enabled', false); p.set('speaker-enabled', false); p.set('recording-enabled', true);
  p.byId('session-name').value = 'Independent recording';
  assert.equal(p.byId('start').disabled, false);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
  assert.equal(p.byId('session-state').textContent, 'Sessão ativa');
  assert.match(p.byId('session-identity').textContent, /Independent recording/);
  assert.equal(p.config().recording.enabled, true);
  assert.equal(p.config().transcription.enabled, false);
  for (const route of ['microphone', 'speaker']) {
    assert.equal(p.byId(`${route}-state`).textContent, 'Roteamento inativo');
    assert.equal(p.byId(`${route}-signal`).textContent, '—');
    assert.equal(p.byId(`${route}-input`).value, 0);
    assert.equal(p.byId(`${route}-output`).value, 0);
  }
  p.byId('stop').click();
  await settle(() => p.byId('stop').hidden);
  assert.equal(p.byId('session-state').textContent, 'Roteamento inativo');
  assert.equal(p.config().recording.enabled, true);
});

test('an unconfigured or inactive microphone never inherits activity from the working speaker or session', async t => {
  const p = await page(t);
  p.set('speaker-enabled', false);
  p.routeStatus('speaker', { state: 'passthrough', input_level: 0.6, output_level: 0.4 });
  for (const running of [false, true]) {
    if (running) {
      p.byId('start').click();
      await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
    }
    for (const state of ['stopped', 'error', 'failed', 'disabled', 'unconfigured']) {
      p.routeStatus('microphone', { state, input_level: 0.9, output_level: 0.8 });
      await p.poll();
      assert.equal(p.byId('microphone-signal').textContent, '—', `${state}, running=${running}`);
      assert.equal(p.byId('microphone-input').value, 0);
      assert.equal(p.byId('microphone-output').value, 0);
      assert.equal(p.byId('microphone-output-db').textContent, '−∞ dB');
      assert.equal(p.byId('speaker-state').textContent, 'Áudio original');
      assert.equal(p.byId('speaker-input').value, 0.6);
      assert.equal(p.byId('speaker-signal').textContent, 'Original');
    }
    assert.equal(p.byId('microphone-state').textContent, 'Dispositivos não configurados');
  }
  await chooseInterface(p, 'en');
  assert.equal(p.byId('microphone-state').textContent, 'Devices not configured');
  assert.equal(p.byId('microphone-signal').textContent, '—');
  assert.equal(p.byId('session-state').textContent, 'Session active');
});

test('waiting endpoint labels follow the interface language and real device errors remain visible', async t => {
  const p = await page(t, { language: 'en' });
  p.routing(false);
  for (const route of ['microphone', 'speaker']) p.routeStatus(route, { state: 'waiting_for_app' });
  await p.poll();
  assert.equal(p.byId('session-state').textContent, 'Routing inactive');
  assert.equal(p.byId('speaker-state').textContent, 'Routing inactive');
  assert.match(p.byId('speaker-state').title, /virtual output/);
  await chooseInterface(p, 'pt');
  assert.equal(p.byId('session-state').textContent, 'Roteamento inativo');
  assert.match(p.byId('speaker-state').title, /saída virtual/);
  assert.match(p.byId('microphone-signal').parentElement.getAttribute('aria-label'), /microfone virtual ser o padrão do sistema ou ser usado por um aplicativo/);
  p.routeStatus('speaker', { state: 'waiting_for_app', device_error: 'Device disconnected', input_level: 1 });
  await p.poll();
  assert.equal(p.byId('speaker-state').textContent, 'Dispositivo indisponível');
  assert.equal(p.byId('speaker-input').value, 0);
  assert.equal(p.byId('speaker-signal').textContent, '—');
  assert.equal(p.byId('speaker-state').title, '');
});

async function chooseInterface(p, language) {
  p.byId('interface-language').value = language;
  p.byId('interface-language').dispatchEvent(new p.window.Event('change', { bubbles: true }));
  await settle(() => !p.byId('interface-language').disabled && p.config().interface.language === language);
}

test('system interface locale comes from the server with English fallback, independent of browser and spoken languages', async t => {
  const p = await page(t, { systemLocale: 'ja-JP' });
  assert.equal(p.doc.documentElement.lang, 'en');
  assert.equal(p.byId('interface-language').value, 'system');
  assert.equal(p.byId('page-title').textContent, 'Audio routing');
  assert.equal(p.byId('session-state').textContent, 'Original audio');
  assert.equal(p.byId('session-name').placeholder, 'e.g. Team meeting');
  assert.equal(p.byId('microphone-source_language').value, 'Automatic');
  assert.equal(p.byId('microphone-target_language').value, 'en-US');
  assert.equal(p.byId('microphone-prompt').placeholder, 'e.g. Use natural language and preserve technical terms.');
  assert.equal(p.doc.querySelector('.brand').getAttribute('aria-label'), 'Babel, home');
  assert.equal(p.doc.querySelector('a[href="/help/configuration"]').hreflang, 'en');
  assert.equal(p.calls.filter(call => call.path === '/api/interface' && call.options.method === 'PUT').length, 0);
  assert.equal(p.config().interface.language, 'system', 'locale detection must not save an explicit language');
});

test('explicit Portuguese overrides an English OS and missing translation keys fall back to English', async t => {
  const p = await page(t, { language: 'pt', systemLocale: 'en-US', omitTranslation: 'workspace.routing_title' });
  assert.equal(p.doc.documentElement.lang, 'pt');
  assert.equal(p.byId('page-title').textContent, JSON.parse(fs.readFileSync(path.join(__dirname, 'locales/en.json'), 'utf8'))['workspace.routing_title']);
  assert.equal(p.byId('session-state').textContent, 'Áudio original');
  assert.equal(p.byId('start').textContent.includes('Iniciar sessão'), true);
  assert.equal(p.window.BabelI18n.number(12345.6), '12.345,6');
  await chooseInterface(p, 'en');
  assert.equal(p.window.BabelI18n.number(12345.6), '12,345.6');
  assert.match(p.window.BabelI18n.date(new Date('2026-09-29T12:00:00Z'), { month: 'long', timeZone: 'UTC' }), /September/);
});

test('interface switching preserves drafts, native nodes, focus and config revisions', async t => {
  const p = await page(t);
  p.set('speaker-target_language', 'ja-JP');
  p.set('microphone-capture_device', '');
  p.set('session-name', 'Sessão que fica');
  p.byId('credential-gemini').value = 'secret-draft-not-saved';
  p.byId('autostart-enabled').click();
  const draft = p.byId('session-name'); draft.focus();
  const originalFocused = p.doc.activeElement;
  await chooseInterface(p, 'en');
  assert.equal(p.doc.documentElement.lang, 'en');
  assert.equal(p.doc.activeElement, originalFocused);
  assert.equal(draft.value, 'Sessão que fica');
  assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
  assert.equal(p.byId('microphone-capture_device').value, '');
  assert.equal(p.byId('session-name').value, 'Sessão que fica');
  assert.equal(p.byId('credential-gemini').value, 'secret-draft-not-saved');
  assert.equal(p.byId('autostart-enabled').checked, true, 'unsaved startup toggle survives');
  assert.equal(p.byId('save-state').textContent, 'Unsaved settings');
  assert.equal(p.config().speaker.target_language, 'pt-BR', 'changing interface must not save unrelated drafts');
  assert.equal(p.byId('config-conflict').hidden, true);
  const languageWrite = p.calls.find(call => call.path === '/api/interface' && call.options.method === 'PUT');
  assert.deepEqual(languageWrite.body, { language: 'en' });
  assert.equal(languageWrite.options.headers['If-Match'], '"0"');
  p.byId('save').click();
  await settle(() => p.byId('notice').textContent === 'Settings saved.');
  assert.equal(p.config().interface.language, 'en');
  assert.equal(p.calls.find(call => call.path === '/api/config' && call.options.method === 'PUT').options.headers['If-Match'], '"1"');
  assert.equal(p.byId('notice').textContent, 'Settings saved.');
});

test('interface can change during a running session without starting, stopping or saving audio settings', async t => {
  const p = await page(t);
  p.set('session-name', 'Reunião original');
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
  assert.equal(p.byId('settings').disabled, true);
  assert.equal(p.byId('interface-language').disabled, false);
  const before = p.calls.length;
  await chooseInterface(p, 'en');
  assert.equal(p.byId('settings').disabled, true);
  assert.equal(p.byId('stop').hidden, false);
  assert.equal(p.byId('session-state').textContent, 'Session active');
  assert.equal(p.byId('session-identity').textContent, 'Session: Reunião original');
  assert.equal(p.byId('microphone-capture_device').value, 'physical-mic');
  assert.equal(p.calls.slice(before).some(call => ['/api/start', '/api/stop', '/api/config'].includes(call.path)), false);
  await p.poll();
  assert.equal(p.byId('config-conflict').hidden, true);
  await chooseInterface(p, 'system');
  assert.equal(p.doc.documentElement.lang, 'pt');
  assert.equal(p.byId('session-identity').textContent, 'Sessão: Reunião original');
});

test('external locale updates sync clean panels and locale writes cannot bypass a stale draft revision', async t => {
  const p = await page(t);
  p.externalChange(config => { config.interface.language = 'en'; });
  await p.poll();
  assert.equal(p.doc.documentElement.lang, 'en');
  assert.equal(p.byId('interface-language').value, 'en');
  p.set('speaker-target_language', 'ja-JP');
  p.externalChange(config => { config.microphone.capture_device = 'new-tray-mic'; });
  p.byId('interface-language').value = 'pt';
  p.byId('interface-language').dispatchEvent(new p.window.Event('change', { bubbles: true }));
  await settle(() => !p.byId('config-conflict').hidden);
  assert.equal(p.doc.documentElement.lang, 'en');
  assert.equal(p.config().interface.language, 'en');
  assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
  assert.equal(p.byId('save').disabled, true);
});

test('catalogs cover static and dynamic keys and preserve interpolation parameters', () => {
  const en = JSON.parse(fs.readFileSync(path.join(__dirname, 'locales/en.json'), 'utf8'));
  const pt = JSON.parse(fs.readFileSync(path.join(__dirname, 'locales/pt.json'), 'utf8'));
  assert.deepEqual(Object.keys(en).sort(), Object.keys(pt).sort());
  for (const key of Object.keys(en)) {
    const parameters = value => [...value.matchAll(/\{([a-zA-Z_][a-zA-Z0-9_]*)\}/g)].map(match => match[1]).sort();
    assert.deepEqual(parameters(en[key]), parameters(pt[key]), `placeholder mismatch: ${key}`);
  }
  const html = fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8');
  const js = fs.readFileSync(path.join(__dirname, 'app.js'), 'utf8');
  for (const match of html.matchAll(/data-i18n(?:-[\w-]+)?="([\w.]+)"/g)) assert.equal(typeof en[match[1]], 'string', match[1]);
  for (const match of js.matchAll(/\bt\(["']([\w.]+)["']/g)) assert.equal(typeof en[match[1]], 'string', match[1]);
  assert.equal(js.includes("navigator.language"), false, 'OS preference must come from the host, not the browser');
});

test('Linux device management follows the host API even when the browser identifies as Windows', async t => {
  const p = await page(t, { platform: 'linux', userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64)' });
  assert.equal(p.byId('platform-summary'), null);
  assert.equal(p.byId('install-devices').hidden, false);
  assert.equal(p.byId('install-devices').disabled, false);
  assert.equal(p.byId('uninstall-devices').hidden, false);
  assert.equal(p.byId('device-external-guide').hidden, true);
  assert.equal(p.byId('platform-native-guide'), null);
  assert.match(p.byId('platform-device-setup').textContent, /Babel_Microphone.*Babel_Speaker/);
  assert.match(p.byId('platform-device-permissions').textContent, /pactl, parec e pacat/);
  assert.match(p.byId('platform-autostart').textContent, /XDG/);
  assert.equal(p.byId('microphone-virtual-label').textContent, 'Babel_Microphone');
  p.byId('install-devices').click();
  await settle(() => p.calls.some(call => call.path === '/api/virtual/install') && !p.byId('install-devices').disabled);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.byId('install-devices').disabled, true);
  assert.equal(p.byId('uninstall-devices').disabled, true);
  assert.equal(p.byId('refresh-devices').disabled, false);
});

test('macOS shows Babel driver setup, permissions and LaunchAgent instead of Linux device actions', async t => {
  const p = await page(t, { platform: 'macos', userAgent: 'Mozilla/5.0 (X11; Linux x86_64)' });
  assert.equal(p.byId('platform-summary'), null);
  assert.equal(p.byId('install-devices').hidden, true); assert.equal(p.byId('install-devices').disabled, true);
  assert.equal(p.byId('uninstall-devices').hidden, true); assert.equal(p.byId('uninstall-devices').disabled, true);
  assert.equal(p.byId('device-external-guide').hidden, false);
  assert.match(p.byId('platform-device-setup').textContent, /Babel Microphone.*Babel Speaker/);
  assert.match(p.byId('platform-device-permissions').textContent, /Privacidade e Segurança.*Microfone/);
  assert.match(p.byId('platform-autostart').textContent, /LaunchAgent/);
  assert.equal(p.byId('microphone-virtual-label').textContent, 'Babel Microphone');
  assert.equal(p.byId('speaker-virtual-label').textContent, 'Babel Speaker');
  assert.equal(p.byId('platform-guide').getAttribute('href'), '/help/platforms');
  assert.equal(p.byId('device-external-guide').getAttribute('href'), '/help/native-drivers');
  assert.equal(p.byId('refresh-devices').disabled, false);
  assert.equal(p.calls.some(call => call.path.startsWith('/api/virtual/')), false);
});

test('Windows displays cable directions, microphone privacy and per-user startup without changing drafts', async t => {
  const p = await page(t, { platform: 'windows', userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X)' });
  assert.equal(p.byId('platform-summary'), null);
  assert.match(p.byId('platform-device-setup').textContent, /Babel Microphone.*Babel Speaker/);
  assert.match(p.byId('platform-device-details').textContent, /Babel Microphone Feed.*Babel Speaker Monitor/);
  assert.match(p.byId('platform-device-permissions').textContent, /aplicativos desktop.*Windows/);
  assert.match(p.byId('platform-autostart').textContent, /Run.*Registro/);
  assert.equal(p.byId('install-devices').hidden, true); assert.equal(p.byId('uninstall-devices').disabled, true);
  assert.equal(p.byId('microphone-virtual-label').textContent, 'Microfone virtual');
  assert.equal(p.byId('speaker-virtual-label').textContent, 'Saída virtual');
  p.set('speaker-target_language', 'ja-JP');
  p.byId('autostart-enabled').click();
  await chooseInterface(p, 'en');
  assert.equal(p.byId('platform-summary'), null);
  assert.match(p.byId('platform-device-permissions').textContent, /desktop apps in Windows/);
  assert.match(p.byId('platform-autostart').textContent, /Run registry/);
  assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
  assert.equal(p.byId('autostart-enabled').checked, true);
  p.window.BabelI18n.apply();
  assert.equal(p.byId('platform-summary'), null);
  assert.match(p.byId('platform-device-setup').textContent, /Babel Microphone/);
  p.byId('refresh-devices').click();
  await settle(() => p.byId('notice').textContent === 'Device list refreshed.');
  assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
  assert.equal(p.byId('autostart-enabled').checked, true);
  assert.equal(p.calls.some(call => call.path === '/api/config' && call.options.method === 'PUT'), false);
});

test('unknown or unavailable host metadata never assumes Linux and refresh recovers without losing edits', async t => {
  const unknown = await page(t, { platform: 'unknown' });
  assert.equal(unknown.byId('platform-summary'), null);
  assert.equal(unknown.byId('install-devices').hidden, true);
  assert.equal(unknown.byId('uninstall-devices').disabled, true);
  assert.equal(unknown.byId('autostart-enabled').disabled, true);
  assert.equal(unknown.byId('microphone-virtual-label').textContent, 'Microfone virtual');
  assert.equal(unknown.byId('speaker-virtual-label').textContent, 'Saída virtual');
  assert.equal(unknown.byId('refresh-devices').disabled, false);
  assert.equal(unknown.byId('platform-native-guide'), null);
  const p = await page(t, { platform: 'linux', platformFailure: true });
  assert.equal(p.byId('platform-summary'), null);
  assert.equal(p.byId('platform-error').hidden, false);
  assert.equal(p.byId('install-devices').hidden, true);
  assert.equal(p.byId('install-devices').disabled, true);
  p.set('speaker-target_language', 'es-ES');
  await chooseInterface(p, 'en');
  assert.equal(p.byId('platform-summary'), null);
  assert.equal(p.byId('install-devices').disabled, true);
  p.platformFailure(false); p.byId('refresh-devices').click();
  await settle(() => p.byId('install-devices').hidden === false && !p.byId('install-devices').disabled);
  assert.equal(p.byId('platform-summary'), null);
  assert.equal(p.byId('platform-error').hidden, true);
  assert.equal(p.byId('speaker-target_language').value, 'es-ES');
  assert.equal(p.calls.some(call => call.path.startsWith('/api/virtual/')), false);
});

test('native Babel guide maps both independent paths, links local build guidance and survives locale changes', async t => {
  for (const os of ['macos', 'windows']) {
    const p = await page(t, { platform: os });
    const guide = p.byId('platform-native-guide');
    assert.equal(guide.closest('#workspace-settings') !== null, true);
    assert.equal(guide.hidden, false);
    assert.equal(guide.open, false);
    assert.match(guide.querySelector('caption').textContent, /dois caminhos independentes/);
    const cells = [...guide.querySelectorAll('tbody td')].map(cell => cell.textContent);
    assert.equal(cells.length, 6);
    assert.deepEqual(cells.slice(1, 5), os === 'macos'
      ? ['Babel Microphone', 'Babel Microphone', 'Babel Speaker', 'Babel Speaker']
      : ['Babel Microphone Feed', 'Babel Microphone', 'Babel Speaker', 'Babel Speaker Monitor']);
    assert.equal(guide.querySelectorAll('tbody th[scope="row"]').length, 6);
    assert.match(guide.textContent, /pacote de distribuição assinado.*validação em hardware.*pendentes/);
    assert.match(guide.textContent, os === 'macos' ? /macOS 14\.2.*BlackHole.*opcionais/ : /Babel Audio v1.*WASAPI.*VB-CABLE.*opcionais/);
    const links = [...guide.querySelectorAll('a')];
    assert.deepEqual(links.map(link => new URL(link.href).pathname), ['/help/native-drivers', '/help/platforms']);
    for (const link of links) {
      assert.equal(link.target, '_blank');
      assert.equal(link.rel, 'noopener noreferrer');
      assert.equal(new URL(link.href).origin, p.window.location.origin);
    }
    assert.equal(guide.querySelector('button, input, select'), null, 'guide cannot perform privileged setup');
    guide.open = true;
    p.set('speaker-target_language', 'ja-JP');
    await chooseInterface(p, 'en');
    assert.equal(p.byId('platform-native-guide'), guide);
    assert.equal(guide.open, true);
    assert.match(guide.querySelector('summary').textContent, /Babel driver installation/);
    assert.match(guide.textContent, /does not execute or elevate/);
    assert.match(guide.textContent, os === 'macos' ? /macOS 14\.2.*diagnostic/ : /ASIO.*have not been validated/);
    assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
    assert.equal(p.calls.some(call => call.path.startsWith('/api/virtual/')), false);
    assert.equal(p.calls.some(call => call.path === '/api/config' && call.options.method === 'PUT'), false);
  }
});

test('native guide hides when host metadata fails and recovers without reinstalling or losing drafts', async t => {
  const p = await page(t, { platform: 'windows' });
  const guide = p.byId('platform-native-guide');
  guide.open = true;
  p.set('speaker-target_language', 'ja-JP');
  p.platformFailure(true); p.byId('refresh-devices').click();
  await settle(() => guide.hidden);
  assert.equal(p.byId('install-devices').hidden, true);
  p.platformFailure(false); p.byId('refresh-devices').click();
  await settle(() => !guide.hidden && !p.byId('refresh-devices').disabled);
  assert.equal(guide.open, true);
  assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
  assert.equal(p.calls.some(call => call.path.startsWith('/api/virtual/')), false);
});

test('recognizers, languages and STT profiles save independently from translation', async t => {
  const p = await page(t, { initialChange: cfg => { cfg.providers.openai.transcription_model = 'legacy-sts-recognition'; } });
  p.set('transcription-enabled', true);
  p.set('stt-microphone-provider', 'deepgram');
  p.set('stt-microphone-language', 'pt-BR');
  p.set('stt-speaker-provider', 'whisper');
  p.set('stt-speaker-language', 'en-US');
  p.set('stt-profile-deepgram-model', 'nova-3-fixture');
  p.set('stt-profile-deepgram-diarize', true);
  p.set('stt-profile-deepgram-punctuate', false);
  p.set('stt-profile-whisper-endpoint', 'http://127.0.0.1:43219/inference');
  p.set('microphone-provider', 'openai');
  p.set('microphone-source_language', 'fr');
  p.set('speaker-provider', 'local');
  assert.equal(p.byId('stt-microphone-provider').value, 'deepgram');
  assert.equal(p.byId('stt-microphone-language').value, 'pt-BR');
  assert.deepEqual([...p.byId('stt-microphone-provider').options].map(o => o.value), ['gemini','openai','deepgram','whisper']);
  p.byId('save').click();
  await settle(() => p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'));
  const cfg = p.config();
  assert.deepEqual(cfg.transcription.microphone_recognition, { provider: 'deepgram', language: 'pt-BR' });
  assert.deepEqual(cfg.transcription.speaker_recognition, { provider: 'whisper', language: 'en-US' });
  assert.equal(cfg.transcription.providers.deepgram.model, 'nova-3-fixture');
  assert.equal(cfg.transcription.providers.deepgram.diarize, true);
  assert.equal(cfg.transcription.providers.deepgram.punctuate, false);
  assert.equal(cfg.microphone.provider, 'openai');
  assert.equal(cfg.speaker.provider, 'local');
  assert.equal(Object.hasOwn(cfg.speaker, 'voice'), false);
  assert.equal(cfg.providers.openai.transcription_model, 'legacy-sts-recognition');
  assert.equal(cfg.providers.gemini.model, 'gemini-3.5-live-translate-preview');
  assert.equal(cfg.transcription.providers.gemini.model, 'gemini-3.5-transcribe-live');
  assert.equal(p.byId('profile-openai-transcription_model').closest('label').hidden, true);
  assert.equal(p.calls.some(c => ['/api/start','/api/voices'].includes(c.path)), false);
});

test('STT credentials use their own profile name and never enter saved settings', async t => {
  const p = await page(t);
  p.set('profile-gemini-api_key_env', 'TRANSLATION_GEMINI_KEY');
  p.set('stt-profile-gemini-api_key_env', 'TRANSCRIPTION_GEMINI_KEY');
  p.byId('credential-gemini').value = 'translation-fixture-secret';
  p.doc.querySelector('.credential-apply[data-provider="gemini"]').click();
  await settle(() => p.calls.some(c => c.path === '/api/credentials' && c.body?.api_key_env === 'TRANSLATION_GEMINI_KEY'));
  await settle(() => !p.byId('settings').disabled);
  p.byId('stt-credential-gemini').value = 'transcription-fixture-secret';
  p.doc.querySelector('.stt-credential-apply[data-provider="gemini"]').click();
  await settle(() => p.calls.some(c => c.path === '/api/credentials' && c.body?.api_key_env === 'TRANSCRIPTION_GEMINI_KEY'));
  await settle(() => !p.byId('settings').disabled);
  assert.equal(p.byId('stt-credential-gemini').value, '');
  const applied = p.calls.filter(c => c.path === '/api/credentials' && c.options.method === 'POST');
  assert.deepEqual(applied.map(c => c.body), [{ api_key_env: 'TRANSLATION_GEMINI_KEY', key: 'translation-fixture-secret' }, { api_key_env: 'TRANSCRIPTION_GEMINI_KEY', key: 'transcription-fixture-secret' }]);
  p.byId('save').click();
  await settle(() => p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'));
  assert.equal(p.config().transcription.providers.gemini.api_key_env, 'TRANSCRIPTION_GEMINI_KEY');
  assert.equal(p.config().providers.gemini.api_key_env, 'TRANSLATION_GEMINI_KEY');
  assert.doesNotMatch(JSON.stringify(p.config()), /fixture-secret/);
  await settle(() => !p.byId('settings').disabled);
  p.doc.querySelector('.stt-credential-clear[data-provider="gemini"]').click();
  await settle(() => p.calls.some(c => c.path === '/api/credentials/clear'));
  assert.deepEqual(p.calls.find(c => c.path === '/api/credentials/clear').body, { api_key_env: 'TRANSCRIPTION_GEMINI_KEY' });
});

test('STT validation reveals the correct profile without changing the translation profile selector', async t => {
  const p = await page(t);
  p.set('profile-selector', 'openai');
  p.byId('profile-selector').dispatchEvent(new p.window.Event('change', { bubbles: true }));
  p.set('profile-openai-model', 'my-sts-draft');
  p.set('transcription-enabled', true);
  p.set('transcription-speaker', false);
  p.set('stt-microphone-provider', 'deepgram');
  p.set('stt-profile-deepgram-model', '');
  p.byId('nav-translation').click();
  p.byId('save').click();
  assert.equal(p.window.BabelWorkspace.current, 'transcription');
  assert.equal(p.byId('stt-profile-selector').value, 'deepgram');
  assert.equal(p.byId('stt-profile-deepgram').hidden, false);
  assert.equal(p.doc.activeElement, p.byId('stt-profile-deepgram-model'));
  assert.equal(p.byId('profile-selector').value, 'openai');
  assert.equal(p.byId('profile-openai-model').value, 'my-sts-draft');
  assert.equal(p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'), false);
  p.set('stt-profile-deepgram-model', 'nova-3');
  p.byId('save').click();
  await settle(() => p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'));
});

test('built-in Whisper transcription needs no external endpoint or credentials and preserves STS settings', async t => {
  const p = await page(t);
  p.set('microphone-enabled', false);
  p.set('speaker-enabled', false);
  p.set('stt-microphone-provider', 'whisper');
  p.set('transcription-enabled', true);
  p.set('transcription-speaker', false);
  assert.equal(p.byId('stt-profile-whisper-endpoint').value, 'auto');
  assert.equal(p.byId('stt-profile-whisper-endpoint').required, false);
  assert.equal(p.byId('stt-profile-whisper-endpoint').disabled, true);
  assert.equal(p.byId('stt-credential-whisper').disabled, true);
  p.set('stt-profile-whisper-model', 'small');
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.config().transcription.providers.whisper.endpoint, 'auto');
  assert.equal(p.config().transcription.providers.whisper.model, 'small');
  assert.equal(p.config().providers.local.whisper_model, 'base');
  assert.equal(p.config().providers.local.whisper_endpoint, 'auto');
});

test('external Whisper requires an explicit endpoint and mode changes preserve endpoint drafts', async t => {
  const p = await page(t);
  p.set('stt-microphone-provider', 'whisper');
  p.set('transcription-enabled', true);
  p.set('transcription-speaker', false);
  p.set('stt-profile-whisper-endpoint-mode', 'external');
  assert.equal(p.byId('stt-profile-whisper-endpoint').value, '');
  assert.equal(p.byId('stt-profile-whisper-endpoint').required, true);
  p.byId('start').click();
  assert.equal(p.doc.activeElement, p.byId('stt-profile-whisper-endpoint'));
  assert.equal(p.calls.some(c => c.path === '/api/start'), false);
  p.set('stt-profile-whisper-endpoint', 'http://127.0.0.1:41921/inference');
  p.set('stt-profile-whisper-endpoint-mode', 'auto');
  assert.equal(p.byId('stt-profile-whisper-endpoint').value, 'auto');
  p.set('stt-profile-whisper-endpoint-mode', 'external');
  assert.equal(p.byId('stt-profile-whisper-endpoint').value, 'http://127.0.0.1:41921/inference');
  assert.equal(p.byId('stt-profile-whisper-model').disabled, true);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.config().transcription.providers.whisper.endpoint, 'http://127.0.0.1:41921/inference');
});

test('STT validation accepts exact segment values and reveals language and timing errors in source order', async t => {
  const p = await page(t);
  p.set('transcription-enabled', true); p.set('transcription-speaker', false);
  p.set('stt-microphone-provider', 'whisper');
  p.set('stt-profile-whisper-endpoint-mode', 'external');
  p.set('stt-profile-whisper-endpoint', 'http://127.0.0.1:49213/inference');
  p.set('stt-profile-whisper-segment_ms', 1234);
  p.set('stt-profile-whisper-silence_ms', 1234);
  p.set('stt-profile-whisper-vad_threshold', 0.01234);
  p.set('stt-microphone-language', 'bad language');
  p.byId('nav-settings').click();
  p.byId('configuration').requestSubmit();
  assert.equal(p.window.BabelWorkspace.current, 'transcription');
  assert.equal(p.doc.activeElement, p.byId('stt-microphone-language'));
  assert.equal(p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'), false);
  p.set('stt-microphone-language', 'pt-BR');
  p.byId('save').click();
  assert.equal(p.doc.activeElement, p.byId('stt-profile-whisper-silence_ms'));
  assert.equal(p.byId('stt-profile-selector').value, 'whisper');
  assert.equal(p.byId('stt-profile-whisper-silence_ms').validationMessage, p.window.BabelI18n.t('stt.silence_shorter'));
  p.set('stt-profile-whisper-silence_ms', 321);
  p.set('stt-profile-whisper-request_timeout_secs', '');
  p.byId('save').click();
  assert.equal(p.doc.activeElement, p.byId('stt-profile-whisper-request_timeout_secs'));
  p.set('stt-profile-whisper-request_timeout_secs', 25);
  p.byId('save').click();
  await settle(() => p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'));
  assert.deepEqual(p.config().transcription.providers.whisper, { endpoint: 'http://127.0.0.1:49213/inference', model: 'base', api_key_env: '', segment_ms: 1234, silence_ms: 321, vad_threshold: 0.01234, request_timeout_secs: 25 });
});

test('processing summary uses the independent STT selection and ignores disabled translator profiles', async t => {
  const p = await page(t);
  p.set('microphone-enabled', false); p.set('speaker-enabled', false);
  p.set('transcription-enabled', true); p.set('transcription-speaker', false);
  p.set('stt-microphone-provider', 'deepgram');
  assert.equal(p.byId('footer-state').textContent, p.window.BabelI18n.t('ui.session_configured_with_cloud_providers'));
  p.set('stt-microphone-provider', 'whisper');
  assert.equal(p.byId('footer-state').textContent, p.window.BabelI18n.t('ui.session_configured_for_local_processing'));
  p.set('microphone-provider', 'openai');
  assert.equal(p.byId('footer-state').textContent, p.window.BabelI18n.t('ui.session_configured_for_local_processing'));
  p.set('microphone-enabled', true);
  assert.equal(p.byId('footer-state').textContent, p.window.BabelI18n.t('ui.session_configured_with_cloud_providers'));
});

test('STT shortcuts, languages and unsaved secrets survive workspace and interface changes', async t => {
  const p = await page(t);
  p.set('stt-microphone-provider', 'deepgram');
  p.set('stt-microphone-language', 'fr-CA');
  p.set('stt-profile-deepgram-model', 'draft-model');
  p.set('stt-profile-deepgram-api_key_env', 'MY_DEEPGRAM_STT');
  p.byId('stt-credential-deepgram').value = 'unsaved-secret';
  const node = p.byId('stt-profile-deepgram-model');
  p.byId('nav-transcription').click();
  p.doc.querySelector('[data-stt-route-profile="microphone"]').click();
  assert.equal(p.byId('stt-profile-selector').value, 'deepgram');
  assert.equal(p.doc.activeElement, node);
  for (const view of ['translation','recording','settings','transcription']) p.byId(`nav-${view}`).click();
  await p.window.BabelI18n.setLanguage('en');
  p.window.dispatchEvent(new p.window.CustomEvent('babel:languagechange'));
  assert.equal(p.byId('stt-profile-deepgram-model'), node);
  assert.equal(node.value, 'draft-model');
  assert.equal(p.byId('stt-microphone-language').value, 'fr-CA');
  assert.equal(p.byId('stt-credential-deepgram').value, 'unsaved-secret');
  assert.equal(p.byId('stt-profile-deepgram-api_key_env').value, 'MY_DEEPGRAM_STT');
  assert.equal(p.byId('stt-provider-title').textContent, 'Transcription providers');
  assert.equal(p.byId('stt-profile-selector').value, 'deepgram');
  assert.equal(p.calls.some(c => c.options.method === 'PUT' || ['/api/start','/api/stop'].includes(c.path)), false);
});

test('local model preparation reports bounded download progress, failures and recovery in both feature pages', async t => {
  const p = await page(t);
  p.set('microphone-provider', 'local');
  p.set('stt-microphone-provider', 'whisper');
  p.runtime({ phase: 'preparing', download: { name: '<img src=x onerror=alert(1)>', received: 1048576, total: 2097152 }, services: [] });
  await p.poll();
  const boxes = [...p.doc.querySelectorAll('[data-local-runtime]')];
  for (const box of boxes) {
    assert.equal(box.hidden, false);
    assert.equal(box.dataset.phase, 'preparing');
    assert.equal(box.querySelector('progress').value, 0.5);
    assert.match(box.querySelector('[data-local-runtime-download-label]').textContent, /50%/);
    assert.equal(box.querySelector('img'), null);
  }
  p.runtime({ phase: 'preparing', download: { name: 'Whisper Base', received: 1024, total: null }, services: [] });
  await p.poll();
  assert.equal(boxes[0].querySelector('progress').hasAttribute('value'), false);
  p.runtime({ phase: 'error', message: 'Model checksum did not match', download: null, services: [] });
  await p.poll();
  assert.equal(boxes[0].querySelector('[data-local-runtime-error]').textContent, 'Model checksum did not match');
  assert.equal(boxes[0].querySelector('[data-local-runtime-download]').hidden, true);
  p.runtime({ phase: 'ready', download: null, message: null, services: ['whisper'] });
  await p.poll();
  assert.equal(boxes[0].querySelector('[data-local-runtime-error]').hidden, true);
  assert.equal(boxes[0].dataset.phase, 'ready');
  p.runtime({ phase: 'cached', download: null, message: null, services: [] });
  await p.poll();
  assert.equal(boxes[0].dataset.phase, 'cached');
  assert.match(boxes[0].querySelector('[data-local-runtime-summary]').textContent, /uma sessão precisa deles/);
});

test('local runtime folder is either OS-specific absolute or automatic and settings survive saving', async t => {
  for (const [os, good, bad] of [['linux', '/srv/babel-models', 'models'], ['macos', '/Users/me/Babel/models', 'models'], ['windows', 'D:\\Babel\\models', '\\models']]) {
    const p = await page(t, { platform: os });
    p.set('local-runtime-directory', bad);
    assert.equal(p.byId('local-runtime-directory').checkValidity(), false, `${os}: reject relative path`);
    p.byId('save').click();
    assert.equal(p.window.BabelWorkspace.current, 'settings');
    p.set('local-runtime-directory', good);
    assert.equal(p.byId('local-runtime-directory').checkValidity(), true, `${os}: absolute path`);
    p.set('local-runtime-threads', 3);
    p.set('local-runtime-idle-unload', 0);
    assert.equal(p.byId('local-runtime-idle-unload').checkValidity(), false);
    p.set('local-runtime-idle-unload', 90);
    p.byId('save').click();
    await settle(() => p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'));
    assert.equal(p.config().local_runtime.directory, good);
    assert.equal(p.config().local_runtime.threads, 3);
    assert.equal(p.config().local_runtime.idle_unload_secs, 90);
    p.set('local-runtime-directory', '');
    assert.equal(p.byId('local-runtime-directory').checkValidity(), true, `${os}: automatic cache`);
  }
});

test('local translation independently switches managed components and retains external model drafts', async t => {
  const p = await page(t);
  p.set('microphone-provider', 'local');
  p.set('profile-local-ollama_endpoint-mode', 'external');
  p.set('profile-local-ollama_endpoint', 'http://127.0.0.1:43518/api/chat');
  p.set('profile-local-translation_model', 'custom-model');
  p.set('profile-local-translation_api', 'ollama');
  p.set('profile-local-ollama_endpoint-mode', 'auto');
  assert.equal(p.byId('profile-local-translation_model').value, 'qwen3-0.6b');
  assert.equal(p.byId('profile-local-translation_model').readOnly, true);
  p.set('profile-local-ollama_endpoint-mode', 'external');
  assert.equal(p.byId('profile-local-translation_model').value, 'custom-model');
  assert.equal(p.byId('profile-local-ollama_endpoint').value, 'http://127.0.0.1:43518/api/chat');
  assert.equal(p.byId('profile-local-whisper_endpoint').value, 'auto');
  assert.equal(p.byId('profile-local-piper_endpoint').value, 'auto');
  p.set('profile-local-piper_endpoint-mode', 'external');
  p.set('profile-local-piper_endpoint', 'http://127.0.0.1:49111/synthesize');
  p.set('profile-local-piper_endpoint-mode', 'auto');
  p.set('profile-local-piper_endpoint-mode', 'external');
  p.byId('save').click();
  await settle(() => p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'));
  assert.equal(p.config().providers.local.translation_api, 'ollama');
  assert.equal(p.config().providers.local.translation_model, 'custom-model');
  assert.equal(p.config().providers.local.whisper_model, 'base');
});

test('model preparation keeps status polling available and session start can be cancelled', async t => {
  let finishStart;
  const pendingStart = new Promise(resolve => { finishStart = resolve; });
  const p = await page(t, { startRequest: () => pendingStart });
  p.runtime({ phase: 'preparing', download: { name: 'Whisper Base', received: 100, total: 200 }, services: [] });
  p.byId('start').click();
  await settle(() => p.calls.some(c => c.path === '/api/start'));
  assert.equal(p.byId('cancel-start').hidden, false);
  assert.equal(p.byId('cancel-start').disabled, false);
  const pollsBefore = p.calls.filter(c => c.path === '/api/status').length;
  await p.poll();
  assert.equal(p.calls.filter(c => c.path === '/api/status').length, pollsBefore + 1);
  assert.equal(p.doc.querySelector('[data-local-runtime="settings"]').dataset.phase, 'preparing');
  p.byId('cancel-start').click();
  await settle(() => p.calls.some(c => c.path === '/api/stop'));
  finishStart(new Response(JSON.stringify({ error: 'Session preparation cancelled' }), { status: 409, headers: { 'Content-Type': 'application/json' } }));
  await settle(() => !p.byId('start').disabled);
  assert.equal(p.byId('cancel-start').hidden, true);
  assert.equal(p.byId('error').hidden, true);
  assert.equal(p.byId('stop').hidden, true);
});

test('history is opt-in per start with shorter availability and the existing revision check', async t => {
  const p = await page(t, { language: 'en' });
  assert.equal(p.byId('history-include').checked, false);
  assert.equal(p.byId('history-include').disabled, true, 'translation alone cannot consume history');
  assert.equal(p.byId('history-request-minutes').value, '10');
  assert.equal(p.byId('history-duration-minutes').value, '10');
  p.set('recording-enabled', true);
  p.set('recording-speaker', false);
  p.set('history-include', true);
  p.set('session-name', 'Meeting with history');
  assert.match(p.byId('history-availability-hint').textContent, /only the available portion/);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  const start = p.calls.find(call => call.path === '/api/start');
  assert.deepEqual(start.body, { name: 'Meeting with history', history_seconds: 600 });
  assert.equal(start.options.headers['If-Match'], '"1"');
  assert.deepEqual(p.config().history, { enabled: true, duration_secs: 600 });
  assert.equal(p.byId('history-include').checked, false);
  p.byId('stop').click();
  await settle(() => p.byId('stop').hidden);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.calls.filter(call => call.path === '/api/start')[1].body.history_seconds, 0);
});

test('advanced start shows live combined audio without enabling history or changing the requested duration', async t => {
  const p = await page(t, { language: 'en' });
  p.byId('history-start-options').open = true;
  const reading = p.byId('history-buffer-duration');
  assert.equal(reading.textContent, '03 min 00 s');
  assert.equal(reading.getAttribute('aria-live'), 'off', 'avoid speaking a timer every second');
  assert.equal(p.byId('history-include').checked, false);
  assert.equal(p.byId('history-buffer-capacity').textContent, 'Buffer capacity: 10 min 0 s');
  p.set('recording-enabled', true);
  p.set('history-include', true);
  p.set('history-request-minutes', 1.5);
  const writes = p.calls.filter(call => call.options.method !== 'GET').length;
  for (const seconds of [181, 182, 599, 600, 601, 3599, 3600, 95, 0]) {
    // The backend's union is authoritative, never the sum of the two lanes
    // or the retrospective window (which includes time without capture).
    p.history({ enabled: true, capacity_secs: 600, available_secs: 600,
      combined_audio_secs: seconds, microphone_secs: seconds, speaker_secs: seconds });
    await p.poll();
    const duration = `${String(Math.floor(seconds / 60)).padStart(2, '0')} min ${String(seconds % 60).padStart(2, '0')} s`;
    assert.equal(reading.textContent, duration);
    assert.equal(reading.textContent.length, 11, 'digit boundaries keep a stable clock width');
    assert.equal(p.byId('history-start-options').open, true);
    assert.equal(p.byId('history-request-minutes').value, '1.5');
    assert.equal(p.byId('history-include').checked, true);
  }
  assert.equal(p.byId('history-buffer-update').textContent, 'Updated every second');
  assert.equal(p.calls.filter(call => call.options.method !== 'GET').length, writes);
});

test('buffer timer distinguishes disabled, unavailable and recovered status in both interface languages', async t => {
  let offline = false;
  const p = await page(t, { language: 'pt', statusRequest: () => { if (offline) throw new Error('offline fixture'); } });
  assert.equal(p.byId('history-buffer-label').textContent, 'Áudio disponível na memória');
  assert.equal(p.byId('history-buffer-update').textContent, 'Atualizado a cada segundo');
  offline = true;
  await p.poll();
  assert.equal(p.byId('history-buffer-duration').textContent, '—');
  assert.equal(p.byId('history-buffer-update').textContent, 'Informações do buffer indisponíveis no momento');
  offline = false;
  p.history({ enabled: false, capacity_secs: 600, combined_audio_secs: 0, microphone_secs: 0, speaker_secs: 0 });
  await p.poll();
  assert.equal(p.byId('history-buffer-duration').textContent, '00 min 00 s');
  assert.equal(p.byId('history-buffer-update').textContent, 'Buffer desativado');
  assert.equal(p.byId('history-buffer-update').dataset.live, 'false');
  p.history({ enabled: true, capacity_secs: 600, microphone_secs: 100, speaker_secs: 100 });
  await p.poll();
  assert.equal(p.byId('history-buffer-duration').textContent, '—', 'older API cannot supply a combined duration');
  p.history({ enabled: true, capacity_secs: 600, combined_audio_secs: 42, microphone_secs: 42, speaker_secs: 42 });
  await p.poll();
  assert.equal(p.byId('history-buffer-duration').textContent, '00 min 42 s');
  assert.equal(p.byId('history-buffer-update').dataset.live, 'true');
});

test('history request is session-only while retention serializes minutes as seconds', async t => {
  const p = await page(t);
  p.set('transcription-enabled', true);
  p.set('history-duration-minutes', 5.5);
  p.byId('save').click();
  await settle(() => p.byId('save').disabled && !p.byId('start').disabled);
  assert.deepEqual(p.config().history, { enabled: true, duration_secs: 330 });
  p.set('history-include', true);
  p.set('history-request-minutes', 1.5);
  assert.equal(p.byId('save').disabled, true);
  assert.equal(p.byId('history-request-minutes').max, '5.5');
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.deepEqual(p.calls.find(call => call.path === '/api/start').body, { history_seconds: 90 });
  assert.deepEqual(p.config().history, { enabled: true, duration_secs: 330 });
});

test('history validates durations without clamping invalid input or sending a start', async t => {
  const p = await page(t, { language: 'en' });
  p.set('recording-enabled', true);
  p.set('history-include', true);
  for (const value of ['', '0', '-1', '10.01', '0.001', 'abc']) {
    p.set('history-request-minutes', value);
    p.byId('start').click();
    assert.equal(p.calls.some(call => call.path === '/api/start'), false, `invalid request ${value}`);
    assert.equal(p.byId('history-start-options').open, true);
    assert.equal(p.byId('history-request-error').hidden, false);
  }
  p.set('history-request-minutes', 10);
  for (const value of ['', '0', '-1', '61', '0.001']) {
    p.set('history-duration-minutes', value);
    p.byId('save').click();
    assert.equal(p.calls.some(call => call.options.method === 'PUT'), false, `invalid retention ${value}`);
    assert.equal(p.window.BabelWorkspace.current, 'settings');
    assert.equal(p.byId('history-retention-error').hidden, false);
  }
  p.set('history-duration-minutes', 2);
  assert.equal(p.byId('history-request-minutes').value, '10');
  p.byId('start').click();
  assert.equal(p.calls.some(call => call.path === '/api/start'), false);
  assert.match(p.byId('history-request-error').textContent, /at most 2 minutes/);
});

test('history requires available selected recording or STT audio and reports start failures', async t => {
  const p = await page(t, { language: 'en', history: { microphone_secs: 0, speaker_secs: 42 }, startRequest: async () => new Response(JSON.stringify({ error: 'History is no longer available. Start without history.' }), { status: 400, headers: { 'Content-Type': 'application/json' } }) });
  p.set('recording-enabled', true);
  p.set('recording-speaker', false);
  assert.equal(p.byId('history-include').disabled, true);
  p.set('transcription-enabled', true);
  p.set('transcription-microphone', false);
  assert.equal(p.byId('history-include').disabled, false);
  p.set('history-include', true);
  p.byId('start').click();
  await settle(() => !p.byId('error').hidden);
  assert.match(p.byId('error').textContent, /History is no longer available/);
  assert.equal(p.byId('start').disabled, false);
  assert.equal(p.byId('history-include').checked, true);
  p.history({ enabled: true, capacity_secs: 600, available_secs: 0, microphone_secs: 0, speaker_secs: 0 });
  await p.poll();
  p.byId('start').click();
  assert.equal(p.calls.filter(call => call.path === '/api/start').length, 1);
  assert.match(p.byId('error').textContent, /unavailable for the selected/);
  p.set('history-include', false);
  assert.equal(p.byId('history-request-minutes').disabled, true);
});

test('history shortcut and translated advanced controls preserve keyboard access on each host OS', async t => {
  const p = await page(t, { language: 'pt' });
  p.byId('history-start-options').open = true;
  assert.equal(p.byId('history-advanced-label').textContent, 'Opções avançadas de início');
  p.byId('history-settings-link').click();
  assert.equal(p.window.BabelWorkspace.current, 'settings');
  assert.equal(p.doc.activeElement, p.byId('history-enabled'));
  assert.equal(p.byId('history-start-options').open, false);
  p.byId('history-start-options').open = true;
  p.byId('history-start-options').dispatchEvent(new p.window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  assert.equal(p.byId('history-start-options').open, false);
  assert.equal(p.doc.activeElement, p.byId('history-start-options').querySelector('summary'));
  for (const os of ['linux', 'macos', 'windows']) {
    const platformPage = os === 'linux' ? p : await page(t, { platform: os });
    platformPage.set('recording-enabled', true);
    assert.equal(platformPage.byId('history-include').disabled, false, `${os} supports history`);
  }
});

test('history transcription progress follows actual session status and clears on stop', async t => {
  const p = await page(t, { language: 'en', historySession: { history_included_secs: 90, history_transcription_pending: true } });
  assert.equal(p.byId('history-session-status').hidden, true);
  p.set('transcription-enabled', true);
  p.set('history-include', true);
  p.set('history-request-minutes', 1.5);
  p.byId('start').click();
  await settle(() => !p.byId('stop').hidden);
  assert.equal(p.byId('history-session-status').hidden, false);
  assert.equal(p.byId('history-session-status').textContent, 'Transcribing 1 min 30 s of recent history. Live audio continues.');
  p.historySession({ history_included_secs: 90, history_transcription_pending: false });
  await p.poll();
  assert.equal(p.byId('history-session-status').textContent, 'Included 1 min 30 s of recent history.');
  p.byId('stop').click();
  await settle(() => p.byId('stop').hidden);
  assert.equal(p.byId('history-session-status').hidden, true);
});


test('compact multilingual choices save independently for local translation and original transcription', async t => {
  const p = await page(t);
  p.set('microphone-provider', 'local');
  p.set('stt-microphone-provider', 'whisper');
  p.set('profile-local-whisper_model', 'base-q5_1');
  p.set('stt-profile-whisper-model', 'tiny-q5_1');
  p.byId('save').click();
  await settle(() => p.calls.some(c => c.path === '/api/config' && c.options.method === 'PUT'));
  assert.equal(p.config().providers.local.whisper_model, 'base-q5_1');
  assert.equal(p.config().transcription.providers.whisper.model, 'tiny-q5_1');
  assert.equal(p.calls.some(c => ['/api/start', '/api/stop'].includes(c.path)), false);
});


function selectedMicrophoneSource(p) { return p.byId('microphone-capture_device').selectedOptions[0]?.dataset.microphoneSource || 'physical_microphone'; }
function selectSpeakerSource(p, source) {
  const option = p.byId('microphone-capture_device').querySelector(`[data-microphone-source="${source}"]`);
  assert.ok(option, source); p.set('microphone-capture_device', option.value);
  return option.value;
}

test('virtual microphone source preserves the physical device and saved microphone processing across all hosts', async t => {
  for (const os of ['linux', 'macos', 'windows']) {
    const p = await page(t, { platform: os, language: 'en' });
    assert.equal(selectedMicrophoneSource(p), 'physical_microphone');
    assert.equal(p.byId('audio-microphone-source'), null, 'one source selector replaces separate type/device fields');
    p.set('microphone-provider', 'openai');
    p.set('profile-openai-model', 'gpt-realtime');
    p.set('microphone-prompt', 'Keep technical terms');
    p.set('microphone-source_language', 'pt-BR');
    p.set('recording-enabled', true); p.set('transcription-enabled', true);
    const physical = p.byId('microphone-capture_device').value;
    const originalMix = structuredClone(p.config().recording.mix);
    selectSpeakerSource(p, 'speaker_original');
    assert.equal(selectedMicrophoneSource(p), 'speaker_original');
    assert.equal(p.byId('microphone-capture_device').disabled, false, 'the single selector remains available to change sources');
    assert.equal(p.config().microphone.capture_device, physical);
    assert.match(p.byId('microphone-source-label').textContent, /Babel Speaker.*original/);
    assert.match(p.byId('microphone-source-hint').textContent, /voice commands are paused/);
    assert.equal(p.byId('microphone-translation-bypass').hidden, false);
    assert.equal(p.byId('microphone-translation-label').textContent, 'Paused');
    for (const id of ['microphone-enabled', 'microphone-provider', 'microphone-source_language', 'microphone-target_language', 'microphone-prompt', 'microphone-gain', 'transcription-microphone', 'stt-microphone-provider', 'stt-microphone-language', 'recording-microphone', 'recording-microphone-gain', 'recording-microphone-priority', 'recording-ducking']) assert.equal(p.byId(id).disabled, true, id);
    for (const id of ['microphone-enabled', 'recording-microphone', 'transcription-microphone']) assert.equal(p.byId(id).checked, true, 'saved microphone choices are retained');
    assert.equal(p.byId('speaker-enabled').disabled, false);
    assert.equal(p.byId('recording-speaker-gain').disabled, false);
    assert.equal(p.byId('session-translation-scope').textContent, 'Incoming audio only');
    p.byId('save').click(); await settle(() => p.byId('save').disabled && !p.byId('settings').disabled);
    assert.equal(p.config().audio.microphone_source, 'speaker_original');
    assert.equal(p.config().microphone.capture_device, physical);
    assert.equal(p.config().microphone.enabled, true);
    assert.equal(p.config().microphone.prompt, 'Keep technical terms');
    assert.equal(p.config().microphone.provider, 'openai');
    assert.equal(p.config().microphone.source_language, 'pt-BR');
    assert.equal(p.config().recording.microphone, true);
    assert.equal(p.config().transcription.microphone, true);
    assert.deepEqual(p.config().recording.mix, originalMix);
    p.byId('refresh-devices').click(); await settle(() => !p.byId('refresh-devices').disabled);
    assert.equal(p.config().microphone.capture_device, physical);
    p.set('microphone-capture_device', physical);
    assert.equal(p.byId('audio-microphone-source'), null, 'one source selector replaces separate type/device fields');
    assert.equal(p.byId('microphone-capture_device').disabled, false);
    assert.equal(p.byId('microphone-enabled').disabled, false);
    assert.equal(p.byId('microphone-enabled').checked, true);
    assert.equal(p.byId('microphone-prompt').value, 'Keep technical terms');
    assert.equal(p.byId('recording-microphone').disabled, false);
    assert.equal(p.byId('recording-microphone-gain').disabled, false);
    assert.equal(p.byId('transcription-microphone').disabled, false);
    assert.equal(p.byId('microphone-translation-bypass').hidden, true);
    assert.equal(p.byId('session-translation-scope').textContent, 'Both directions');
    p.byId('save').click(); await settle(() => p.byId('save').disabled && !p.byId('settings').disabled);
    assert.equal(p.config().audio.microphone_source, 'physical_microphone');
    assert.equal(p.config().microphone.capture_device, physical);
  }
});

test('speaker-original source skips microphone-only session features while quick controls choose eligible audio', async t => {
  const p = await page(t, { language: 'en' });
  p.set('speaker-enabled', false);
  p.set('recording-enabled', true); p.set('recording-speaker', false);
  p.set('transcription-enabled', true); p.set('transcription-speaker', false);
  selectSpeakerSource(p, 'speaker_original');
  assert.equal(p.byId('start').disabled, true);
  for (const feature of ['translation', 'recording', 'transcription']) assert.equal(p.byId(`session-${feature}-enabled`).checked, false);
  assert.equal(p.byId('session-translation-scope').textContent, 'Incoming audio only');
  p.set('session-translation-enabled', true);
  assert.equal(p.byId('speaker-enabled').checked, true);
  assert.equal(p.byId('microphone-enabled').checked, true);
  assert.equal(p.byId('start').disabled, false);
  p.set('session-translation-enabled', false);
  assert.equal(p.byId('speaker-enabled').checked, false);
  assert.equal(p.byId('microphone-enabled').checked, true);
  assert.equal(p.byId('start').disabled, true);
  for (const feature of ['recording', 'transcription']) {
    p.set(`session-${feature}-enabled`, true);
    assert.equal(p.byId(`${feature}-speaker`).checked, true);
    assert.equal(p.byId(`${feature}-microphone`).checked, true);
    assert.equal(p.byId('start').disabled, false);
    p.set(`session-${feature}-enabled`, false);
    assert.equal(p.byId(`${feature}-microphone`).checked, true);
    assert.equal(p.byId('start').disabled, true);
  }
  p.set('session-recording-enabled', true);
  p.byId('start').click(); await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
  assert.equal(p.byId('settings').disabled, true);
  assert.equal(p.byId('microphone-capture_device').matches(':disabled'), true);
  assert.equal(p.config().microphone.enabled, true);
  assert.equal(p.config().speaker.enabled, false);
  assert.equal(p.config().recording.microphone, true);
  assert.equal(p.config().recording.speaker, true);
  assert.equal(p.byId('microphone-signal').textContent, 'Original', 'bypassed microphone settings never label the original mirror as AI');
  assert.match(p.byId('microphone-signal').parentElement.getAttribute('aria-label'), /Original incoming audio from Babel Speaker/);
  assert.equal(p.calls.some(call => call.path.startsWith('/api/agent')), false);
});

test('speaker-original source ignores microphone-only history and preserves bypassed dedicated-model instructions', async t => {
  const p = await page(t, { language: 'en' });
  p.set('microphone-prompt', 'Saved instructions');
  p.set('recording-enabled', true);
  selectSpeakerSource(p, 'speaker_original');
  p.history({ enabled: true, capacity_secs: 600, combined_audio_secs: 90, microphone_secs: 90, speaker_secs: 0 });
  await p.poll();
  assert.equal(p.byId('history-include').disabled, true, 'previous physical microphone audio is not eligible in this routing mode');
  p.history({ enabled: true, capacity_secs: 600, combined_audio_secs: 110, microphone_secs: 90, speaker_secs: 20 });
  await p.poll();
  assert.equal(p.byId('history-include').disabled, false);
  p.byId('save').click(); await settle(() => p.byId('save').disabled && !p.byId('settings').disabled);
  assert.equal(p.config().microphone.prompt, 'Saved instructions', 'a bypassed dedicated provider does not clear stored instructions');
  assert.equal(p.config().microphone.source_language, 'pt-BR');
  p.set('interface-language', 'pt'); p.byId('interface-language').dispatchEvent(new p.window.Event('change', { bubbles: true }));
  await settle(() => p.doc.documentElement.lang === 'pt' && !p.byId('interface-language').disabled);
  assert.equal(selectedMicrophoneSource(p), 'speaker_original');
  assert.equal(p.byId('microphone-translation-label').textContent, 'Pausada');
  assert.match(p.byId('microphone-source-hint').textContent, /comandos de voz ficam pausados/);
  assert.equal(p.byId('microphone-prompt').value, 'Saved instructions');
});

test('legacy configurations default to the physical microphone and external source updates refresh the dashboard', async t => {
  const p = await page(t, { language: 'en', configure: config => { delete config.audio.microphone_source; } });
  assert.equal(selectedMicrophoneSource(p), 'physical_microphone');
  p.externalChange(config => { config.audio.microphone_source = 'speaker_original'; });
  await p.poll();
  assert.equal(selectedMicrophoneSource(p), 'speaker_original');
  assert.equal(p.byId('microphone-capture_device').disabled, false);
  assert.equal(p.byId('session-translation-scope').textContent, 'Incoming audio only');
  p.externalChange(config => { config.audio.microphone_source = 'physical_microphone'; });
  await p.poll();
  assert.equal(p.byId('microphone-capture_device').disabled, false);
  assert.equal(p.byId('microphone-capture_device').value, 'physical-mic');
  assert.equal(p.byId('microphone-enabled').disabled, false);
});


test('incoming audio after translation mirrors speaker processing without enabling microphone processing', async t => {
  const p = await page(t, { language: 'en' });
  p.set('microphone-enabled', false);
  p.set('recording-enabled', true); p.set('transcription-enabled', true);
  selectSpeakerSource(p, 'speaker_output');
  assert.equal(p.byId('microphone-capture_device').disabled, false);
  assert.equal(p.config().microphone.capture_device, 'physical-mic');
  assert.equal(p.byId('microphone-enabled').checked, false);
  assert.equal(p.byId('microphone-enabled').disabled, true);
  assert.equal(p.byId('transcription-microphone').disabled, true);
  assert.equal(p.byId('recording-microphone').disabled, true);
  assert.match(p.byId('microphone-source-hint').textContent, /translated when.*active, original otherwise/);
  assert.match(p.byId('microphone-translation-bypass').textContent, /after any incoming-audio translation/);
  assert.equal(p.byId('session-translation-scope').textContent, 'Incoming audio only');
  p.byId('save').click(); await settle(() => p.byId('save').disabled && !p.byId('settings').disabled);
  assert.equal(p.config().audio.microphone_source, 'speaker_output');
  await p.poll();
  assert.equal(p.byId('microphone-signal').textContent, 'Speaker audio');
  p.byId('start').click(); await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
  assert.equal(p.byId('speaker-signal').textContent, 'AI');
  assert.equal(p.byId('microphone-signal').textContent, 'Translated');
  assert.equal(p.byId('microphone-output-label').textContent, 'Played audio');
  assert.equal(p.byId('microphone-state').textContent, 'After output translation');
  p.routeStatus('speaker', { state: 'passthrough' }); await p.poll();
  assert.equal(p.byId('microphone-signal').textContent, 'Speaker audio');
  assert.equal(p.byId('microphone-state').textContent, 'Audio from Babel Speaker');
  p.routeStatus('speaker', { state: 'running' }); await p.poll();
  assert.equal(p.byId('microphone-signal').textContent, 'Translated');

  assert.equal(p.config().microphone.enabled, false);
  assert.equal(p.config().recording.microphone, true);
  assert.equal(p.config().recording.speaker, true);
  assert.match(p.byId('microphone-signal').parentElement.getAttribute('aria-label'), /Audio played by Babel Speaker/);
  p.byId('stop').click(); await settle(() => p.byId('stop').hidden && !p.byId('settings').disabled);
  assert.equal(p.byId('microphone-signal').textContent, 'Speaker audio');
  p.set('speaker-enabled', false);
  p.byId('start').click(); await settle(() => !p.byId('stop').hidden && !p.byId('stop').disabled);
  assert.equal(p.byId('microphone-signal').textContent, 'Speaker audio');
  assert.equal(p.byId('speaker-signal').textContent, 'Original');
  assert.equal(p.byId('session-translation-enabled').checked, false);
  assert.equal(p.byId('microphone-state').textContent, 'Audio from Babel Speaker');
});


test('the unified source list preserves dirty physical drafts across speaker choices, refreshes and language changes', async t => {
  const p = await page(t, { language: 'en', extraInputDevices: [{ id: 'physical-mic-two', name: 'USB microphone', direction: 'input', is_virtual: false }, { id: 'other-monitor', name: 'Other virtual monitor', direction: 'input', is_virtual: true }] });
  const source = p.byId('microphone-capture_device');
  assert.equal(source.hasAttribute('data-field'), false, 'source tokens are never copied to raw config by generic field collection');
  assert.equal(p.byId('audio-microphone-source'), null);
  assert.deepEqual([...source.options].filter(option => option.dataset.microphoneSource).map(option => option.textContent), ['Babel Speaker — original audio', 'Babel Speaker — after translation']);
  assert.equal([...source.options].some(option => ['babel_speaker.monitor', 'other-monitor'].includes(option.value)), false);
  assert.equal([...p.byId('speaker-capture_device').options].some(option => option.value === 'babel_speaker.monitor'), true, 'the output capture selector is unchanged');
  p.set('microphone-capture_device', 'physical-mic-two');
  const originalToken = selectSpeakerSource(p, 'speaker_original');
  assert.equal(p.config().microphone.capture_device, 'physical-mic', 'unsaved physical drafts do not change persisted settings');
  p.byId('refresh-devices').click(); await settle(() => !p.byId('refresh-devices').disabled);
  assert.equal(selectedMicrophoneSource(p), 'speaker_original');
  p.set('interface-language', 'pt'); p.byId('interface-language').dispatchEvent(new p.window.Event('change', { bubbles: true }));
  await settle(() => p.doc.documentElement.lang === 'pt' && !p.byId('interface-language').disabled);
  assert.equal(source.value, originalToken);
  assert.equal(source.selectedOptions[0].textContent, 'Babel Speaker — áudio original');
  assert.equal([...source.options].find(option => option.value === 'physical-mic-two').textContent, 'USB microphone');
  const postToken = selectSpeakerSource(p, 'speaker_output');
  assert.equal(source.selectedOptions[0].textContent, 'Babel Speaker — após tradução');
  p.byId('save').click(); await settle(() => p.byId('save').disabled && !p.byId('settings').disabled);
  assert.equal(p.config().audio.microphone_source, 'speaker_output');
  assert.equal(p.config().microphone.capture_device, 'physical-mic-two', 'speaker source preserves the last physical draft');
  assert.equal(JSON.stringify(p.config()).includes(originalToken), false);
  assert.equal(JSON.stringify(p.config()).includes(postToken), false);
  p.set('microphone-capture_device', 'physical-mic-two');
  assert.equal(selectedMicrophoneSource(p), 'physical_microphone');
  assert.equal(p.byId('microphone-enabled').disabled, false);
  p.byId('refresh-devices').click(); await settle(() => !p.byId('refresh-devices').disabled);
  assert.equal(source.value, 'physical-mic-two');
  p.byId('save').click(); await settle(() => p.byId('save').disabled && !p.byId('settings').disabled);
  assert.equal(p.config().audio.microphone_source, 'physical_microphone');
  assert.equal(p.config().microphone.capture_device, 'physical-mic-two');
});

test('a legacy virtual capture stays unavailable rather than becoming a selectable physical microphone', async t => {
  const p = await page(t, { language: 'en', configure: config => { config.microphone.capture_device = 'babel_speaker.monitor'; } });
  const source = p.byId('microphone-capture_device');
  assert.equal(source.value, 'babel_speaker.monitor');
  assert.equal(source.selectedOptions[0].disabled, true);
  assert.match(source.selectedOptions[0].textContent, /unavailable/i);
  const token = selectSpeakerSource(p, 'speaker_original');
  p.byId('save').click(); await settle(() => p.byId('save').disabled && !p.byId('settings').disabled);
  assert.equal(p.config().audio.microphone_source, 'speaker_original');
  assert.equal(p.config().microphone.capture_device, 'babel_speaker.monitor', 'legacy raw values are preserved, not replaced by UI tokens');
  assert.equal(JSON.stringify(p.config()).includes(token), false);
  p.externalChange(config => { config.audio.microphone_source = 'speaker_output'; }); await p.poll();
  assert.equal(selectedMicrophoneSource(p), 'speaker_output');
  assert.equal(p.byId('microphone-enabled').disabled, true);
});
