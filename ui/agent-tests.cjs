// Agent tests use only in-memory HTTP fixtures; no microphone, MCP tool or account is contacted.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const { JSDOM } = require('jsdom');

const fixtureIntegration = (id = 'test-server', extra = {}) => ({ id, name: 'Calendar', enabled: true, transport: 'http', command: '', args: [], env: {}, cwd: '', url: 'http://127.0.0.1:8002/mcp', auth: 'bearer', token_env: 'CALENDAR_API_KEY', headers: {}, secret_headers: {}, secret_env: {}, oauth: { client_id: '', client_secret_env: '', scopes: [] }, timeout_secs: 30, allowed_tools: [], ...extra });
const defaults = () => ({ enabled: true, wake_name: 'Babel', services_directory: '', whisper_endpoint: 'auto', whisper_model: 'base-q5_1', local_threads: 2, idle_unload_secs: 60, whisper_language: 'auto', whisper_api_key_env: '', needle_endpoint: 'auto', needle_api_key_env: '', desktop_notifications: true, max_calls: 4, min_confidence: 0.85, silence_ms: 600, max_utterance_ms: 10000, command_window_secs: 8, timeout_secs: 20, vad_threshold: 0.012, integrations: [fixtureIntegration()] });
async function settle(predicate, label = 'condition') {
  for (let i = 0; i < 150; i++) { if (predicate()) return; await new Promise(resolve => setTimeout(resolve, 1)); }
  assert.fail(`Timed out: ${label}`);
}
async function page(t, options = {}) {
  const dom = new JSDOM(fs.readFileSync(path.join(__dirname, 'index.html'), 'utf8'), { url: 'http://127.0.0.1:9473/', runScripts: 'outside-only', pretendToBeVisual: true });
  t.after(async () => { await new Promise(resolve => setImmediate(resolve)); dom.window.close(); });
  const { window } = dom; const doc = window.document; const byId = id => doc.getElementById(id);
  let config = defaults(); options.configure?.(config); let revision = 7; let interval; let oauthAuthenticated = false;
  let status = { phase: 'listening', wake_name: 'Babel', microphone_active: true, sequence: 1, activation_id: 0, command: null, tool: null, result: null, error: null, dropped_frames: 0, whisper_endpoint: null, needle_endpoint: null, ...options.status };
  const calls = []; const secrets = new Set(); const timers = new Map(); let timerId = 0;
  window.structuredClone = structuredClone; window.AbortSignal = AbortSignal;
  window.BabelDashboard = { authorization: () => `Bearer ${'a'.repeat(64)}` };
  window.setInterval = callback => { interval = callback; return 0; };
  window.setTimeout = (callback, delay) => { const id = ++timerId; timers.set(id, { callback, delay }); return id; };
  window.clearTimeout = id => timers.delete(id);
  window.fetch = async (url, request = {}) => {
    const parsed = new URL(url, window.location.origin);
    if (parsed.pathname.startsWith('/locales/')) return new Response(fs.readFileSync(path.join(__dirname, parsed.pathname), 'utf8'), { headers: { 'Content-Type': 'application/json' } });
    const body = request.body ? JSON.parse(request.body) : undefined; calls.push({ path: parsed.pathname, request, body });
    assert.equal(request.headers.Authorization, `Bearer ${'a'.repeat(64)}`);
    const reply = (value, status = 200) => new Response(JSON.stringify(value), { status, headers: { 'Content-Type': 'application/json', ETag: `"${revision}"` } });
    if (request.headers['If-Match'] && request.headers['If-Match'] !== `"${revision}"`) return reply({ error: 'Agent revision conflict' }, 412);
    if (parsed.pathname === '/api/agent' && request.method === 'GET') return reply(config);
    if (parsed.pathname === '/api/agent' && request.method === 'PUT') { config = body; revision++; return reply({ ok: true }); }
    if (parsed.pathname === '/api/agent/status') return reply({ ...status, config_revision: revision });
    if (parsed.pathname === '/api/credentials') return reply({ configured: secrets.has(parsed.searchParams.get('api_key_env')) });
    if (parsed.pathname === '/api/agent/credentials') { secrets.add(body.api_key_env); return reply({ ok: true }); }
    if (parsed.pathname === '/api/agent/credentials/clear') { secrets.delete(body.api_key_env); return reply({ ok: true }); }
    if (parsed.pathname === '/api/agent/integrations/test') return reply({ tools: [{ server_id: 'test-server', name: '<img src=x onerror=alert(1)>', description: 'Find an appointment', input_schema: { type: 'object' } }] });
    if (parsed.pathname === '/api/agent/integrations/status') return reply([{ id: 'test-server', authenticated: oauthAuthenticated }]);
    if (parsed.pathname === '/api/agent/oauth/begin') return reply({ authorization_url: options.authorizationUrl || 'https://accounts.example.test/authorize?state=fixture' });
    if (parsed.pathname === '/api/agent/oauth/disconnect') { oauthAuthenticated = false; return reply({ ok: true }); }
    if (parsed.pathname === '/api/agent/cancel') { status = { ...status, phase: 'listening' }; return reply({ ok: true }); }
    assert.fail(`Unexpected network request: ${parsed.pathname}`);
  };
  window.eval(fs.readFileSync(path.join(__dirname, 'i18n.js'), 'utf8'));
  await window.BabelI18n.setLanguage(options.language || 'pt');
  window.eval(fs.readFileSync(path.join(__dirname, 'agent.js'), 'utf8'));
  window.eval(fs.readFileSync(path.join(__dirname, 'workspace.js'), 'utf8'));
  await settle(() => byId('agent-wake_name') && !byId('agent-fields').disabled && byId('agent-summary').dataset.stage);
  const set = (id, value) => { const input = byId(id); assert.ok(input, id); if (input.type === 'checkbox') input.checked = value; else input.value = value; input.dispatchEvent(new window.Event('input', { bubbles: true })); input.dispatchEvent(new window.Event('change', { bubbles: true })); };
  const save = async () => { byId('agent-form').dispatchEvent(new window.Event('submit', { bubbles: true, cancelable: true })); await settle(() => byId('agent-save').disabled && byId('agent-notice').textContent.includes(window.BabelI18n.language === 'pt' ? 'Ajustes do agente salvos.' : 'Agent settings saved.')); };
  return { window, doc, byId, calls, set, save, timers, expire: () => { const pending = [...timers.values()]; timers.clear(); for (const timer of pending) timer.callback(); }, poll: () => interval(), config: () => config, updateStatus: value => { status = { ...status, ...value }; }, externalChange: update => { update(config); revision++; }, authenticate: () => { oauthAuthenticated = true; } };
}

test('voice settings save independently while the audio/session fieldset is disabled', async t => {
  const p = await page(t);
  assert.equal(p.byId('settings').disabled, true);
  assert.equal(p.byId('agent-fields').disabled, false);
  assert.equal(p.byId('agent-enabled').disabled, false);
  assert.equal(p.byId('agent-enabled').checked, true);
  p.byId('speaker-target_language').value = 'ja-JP';
  assert.equal(p.byId('agent-min_confidence').min, '0');
  assert.equal(p.byId('agent-min_confidence').max, '1');
  assert.equal(p.byId('agent-min_confidence').value, '0.85');
  p.set('agent-wake_name', 'Atlas'); p.set('agent-desktop_notifications', false); p.set('agent-max_calls', 2); p.set('agent-min_confidence', 0.70);
  p.set('agent-enabled', false);
  assert.equal(p.config().enabled, true, 'the toggle only updates the draft before saving');
  const saving = p.save();
  assert.equal(p.byId('agent-enabled').disabled, true, 'the header toggle is also locked during a save');
  await saving;
  assert.equal(p.config().enabled, false);
  assert.equal(p.byId('agent-enabled').disabled, false);
  assert.equal(p.byId('agent-enabled').checked, false);
  assert.equal(p.config().wake_name, 'Atlas'); assert.equal(p.config().desktop_notifications, false); assert.equal(p.config().max_calls, 2); assert.equal(p.config().min_confidence, 0.70);
  assert.equal(p.calls.find(c => c.path === '/api/agent' && c.request.method === 'PUT').request.headers['If-Match'], '"7"');
  assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
  assert.equal(p.calls.some(c => ['/api/config', '/api/start', '/api/stop'].includes(c.path)), false);
  assert.equal(p.calls.some(c => c.path.endsWith('/integrations/test')), false);
  for (const confidence of [0, 1]) {
    p.set('agent-enabled', Boolean(confidence));
    p.set('agent-min_confidence', confidence);
    assert.equal(p.byId('agent-min_confidence').checkValidity(), true);
    await p.save();
    assert.equal(p.config().min_confidence, confidence);
    assert.equal(p.config().enabled, Boolean(confidence));
    assert.equal(p.byId('speaker-target_language').value, 'ja-JP');
  }
});

test('auto services and an optional absolute installation folder save without persisting active ports', async t => {
  const p = await page(t);
  for (const key of ['whisper_endpoint', 'needle_endpoint']) {
    assert.equal(p.byId(`agent-${key}`).type, 'text');
    assert.equal(p.byId(`agent-${key}`).value, 'auto');
    assert.equal(p.byId(`agent-${key}`).checkValidity(), true);
    assert.match(p.byId(`agent-${key}-hint`).textContent, /auto/);
  }
  assert.equal(p.byId('agent-services_directory').required, false);
  assert.equal(p.byId('agent-services_directory').value, '');
  assert.match(p.byId('agent-services_directory-hint').textContent, /absoluta/);
  p.updateStatus({ whisper_endpoint: 'http://127.0.0.1:34123/inference', needle_endpoint: 'http://127.0.0.1:45234/complete' });
  await p.poll();
  const folder = 'C:\\Usuários\\João\\Babel';
  p.set('agent-services_directory', folder);
  p.set('agent-needle_api_key_env', 'LOCAL_NEEDLE_TOKEN');
  p.set('agent-min_confidence', 0);
  await p.save();
  assert.equal(p.config().services_directory, folder);
  assert.equal(p.config().whisper_endpoint, 'auto');
  assert.equal(p.config().needle_endpoint, 'auto');
  assert.equal(p.config().whisper_api_key_env, '');
  assert.match(p.byId('agent-whisper_api_key_env-hint').textContent, /vazio com auto/);
  assert.equal(p.config().needle_api_key_env, 'LOCAL_NEEDLE_TOKEN');
  assert.equal(p.config().min_confidence, 0);
  assert.equal(JSON.stringify(p.calls.filter(c => c.request.method === 'PUT').map(c => c.body)).includes('34123'), false);
  assert.equal(p.byId('agent-whisper-effective-endpoint').textContent, 'http://127.0.0.1:34123/inference');
  p.set('agent-services_directory', ''); await p.save();
  assert.equal(p.config().services_directory, '');
});

test('effective service addresses update from status without replacing manual drafts or executing tools', async t => {
  const p = await page(t, { configure: config => {
    config.whisper_endpoint = 'http://localhost:32222/inference';
    config.needle_endpoint = 'http://localhost:43333/complete';
  } });
  assert.equal(p.byId('agent-whisper-effective-endpoint').tagName, 'OUTPUT');
  assert.equal(p.byId('agent-whisper-effective-endpoint').textContent, 'Sem endereço ativo');
  p.set('agent-whisper_api_key_env', 'LOCAL_ASR_TOKEN');
  p.set('agent-needle_endpoint', 'http://127.0.0.1:44444/complete');
  p.updateStatus({ whisper_endpoint: 'http://127.0.0.1:32111/inference', needle_endpoint: 'http://127.0.0.1:43222/complete' });
  await p.poll();
  assert.equal(p.byId('agent-whisper-effective-endpoint').textContent, 'http://127.0.0.1:32111/inference');
  assert.equal(p.byId('agent-needle-effective-endpoint').textContent, 'http://127.0.0.1:43222/complete');
  assert.equal(p.byId('agent-needle_endpoint').value, 'http://127.0.0.1:44444/complete');
  p.updateStatus({ whisper_endpoint: null, needle_endpoint: '<img src=x onerror=alert(1)>' });
  await p.poll();
  assert.equal(p.byId('agent-whisper-effective-endpoint').textContent, 'Sem endereço ativo');
  assert.equal(p.byId('agent-needle-effective-endpoint').querySelector('img'), null);
  assert.equal(p.byId('agent-needle-effective-endpoint').textContent, '<img src=x onerror=alert(1)>');
  await p.window.BabelI18n.setLanguage('en');
  assert.equal(p.byId('agent-whisper-effective-endpoint').textContent, 'No active address');
  assert.match(p.byId('agent-services_directory-hint').textContent, /absolute/);
  assert.equal(p.byId('agent-needle_endpoint').value, 'http://127.0.0.1:44444/complete');
  await p.save();
  assert.equal(p.config().whisper_endpoint, 'http://localhost:32222/inference');
  assert.equal(p.config().whisper_api_key_env, 'LOCAL_ASR_TOKEN');
  assert.equal(p.config().needle_endpoint, 'http://127.0.0.1:44444/complete');
  assert.equal(p.calls.some(c => ['/api/config', '/api/start', '/api/stop'].includes(c.path) || c.path.includes('tools/call')), false);
});

test('MCP integrations can be added, edited and removed without losing other draft values', async t => {
  const p = await page(t);
  p.byId('mcp-add').click();
  const rows = p.doc.querySelectorAll('.mcp-item'); assert.equal(rows.length, 2);
  const id = rows[1].dataset.integrationId;
  p.set(`mcp-${id}-name`, 'Local files'); p.set(`mcp-${id}-transport`, 'stdio');
  p.set(`mcp-${id}-command`, '/usr/bin/mcp-example'); p.set(`mcp-${id}-args`, '--root\n/home/test files');
  p.set(`mcp-${id}-env`, 'MODE=local\nEMPTY='); p.set(`mcp-${id}-secret_env`, 'API_KEY=FILES_KEY'); p.set(`mcp-${id}-allowed_tools`, 'read_file\nlist_files');
  p.set('mcp-test-server-name', 'Calendar renamed');
  await p.save();
  assert.equal(p.config().integrations.length, 2);
  const saved = p.config().integrations[1]; assert.deepEqual(saved.args, ['--root', '/home/test files']); assert.deepEqual(saved.env, { MODE: 'local', EMPTY: '' }); assert.deepEqual(saved.secret_env, { API_KEY: 'FILES_KEY' }); assert.deepEqual(saved.allowed_tools, ['read_file', 'list_files']);
  p.doc.querySelector('[data-integration-id="test-server"] [data-mcp-action="remove"]').click();
  p.set(`mcp-${id}-name`, 'Files after removing another row'); await p.save();
  assert.equal(p.config().integrations.length, 1); assert.equal(p.config().integrations[0].name, 'Files after removing another row');
  assert.equal(p.calls.some(c => c.path.includes('/integrations/test')), false);
});

test('temporary MCP secrets use separate authenticated APIs and never enter agent config', async t => {
  const p = await page(t);
  p.byId('agent-credential-value').value = 'secret-only-in-memory';
  p.doc.querySelector('#agent-credentials button').click();
  await settle(() => p.byId('agent-notice').textContent.includes('Segredo temporário aplicado'));
  assert.equal(p.byId('agent-credential-value').value, '');
  assert.equal(p.calls.find(c => c.path === '/api/agent/credentials').body.key, 'secret-only-in-memory');
  assert.equal(JSON.stringify(p.config()).includes('secret-only-in-memory'), false);
  p.set('agent-wake_name', 'Babel two'); await p.save();
  assert.equal(JSON.stringify(p.calls.find(c => c.path === '/api/agent' && c.request.method === 'PUT').body).includes('secret-only-in-memory'), false);
  p.doc.querySelectorAll('#agent-credentials button')[1].click();
  await settle(() => p.byId('agent-notice').textContent.includes('Segredo temporário removido'));
});

test('tool discovery is explicit, safe text and disabled for unsaved integrations', async t => {
  const p = await page(t);
  const testButton = () => p.doc.querySelector('[data-mcp-action="test"]');
  p.set('mcp-test-server-name', 'New draft name'); assert.equal(testButton().disabled, true);
  await p.save(); assert.equal(testButton().disabled, false);
  testButton().click(); await settle(() => p.doc.querySelector('.mcp-tools').childElementCount === 1);
  assert.match(p.doc.querySelector('.mcp-message').textContent, /nenhuma executada/);
  assert.equal(p.doc.querySelector('.mcp-tools').querySelectorAll('img').length, 0);
  assert.match(p.doc.querySelector('.mcp-tools').textContent, /<img/);
  assert.deepEqual(p.calls.find(c => c.path.endsWith('/integrations/test')).body, { id: 'test-server' });
  assert.equal(p.calls.some(c => c.path.includes('tools/call')), false);
});

test('OAuth requires a connect action and explicit authorization link; returning updates account state', async t => {
  const p = await page(t, { configure: config => { config.integrations[0].auth = 'oauth'; } });
  assert.equal(p.calls.some(c => c.path.includes('/oauth/')), false);
  p.doc.querySelector('[data-mcp-action="connect"]').click();
  await settle(() => !p.doc.querySelector('.mcp-auth-link').hidden);
  const link = p.doc.querySelector('.mcp-auth-link'); assert.equal(link.protocol, 'https:'); assert.equal(link.target, '_blank'); assert.equal(link.rel, 'noopener noreferrer');
  p.authenticate(); for (let i = 0; i < 5; i++) await p.poll();
  assert.equal(link.hidden, true); assert.match(p.doc.querySelector('.mcp-message').textContent, /Conta conectada/);
  p.doc.querySelector('[data-mcp-action="disconnect"]').click(); await settle(() => p.doc.querySelector('.mcp-message').textContent.includes('Conta desconectada'));
});

test('OAuth rejects executable authorization URLs', async t => {
  const p = await page(t, { configure: config => { config.integrations[0].auth = 'oauth'; }, authorizationUrl: 'javascript:alert(1)' });
  p.doc.querySelector('[data-mcp-action="connect"]').click();
  await settle(() => p.doc.querySelector('.mcp-message').textContent.includes('inválida'));
  assert.equal(p.doc.querySelector('.mcp-auth-link').hidden, true);
});

test('activation, processing, completion, failure and cancel show real status without interpreting tool HTML', async t => {
  const p = await page(t);
  assert.equal(p.byId('agent-activity').hidden, true);
  p.updateStatus({ phase: 'activated', activation_id: 1 }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, false); assert.match(p.byId('agent-activity-title').textContent, /Escutando/);
  p.updateStatus({ phase: 'deciding', command: '<img src=x> List appointments' }); await p.poll();
  assert.match(p.byId('agent-activity-title').textContent, /Escolhendo/); assert.equal(p.byId('agent-activity-command').querySelector('img'), null);
  p.updateStatus({ phase: 'executing', tool: 'calendar.list' }); await p.poll(); assert.match(p.byId('agent-activity-tool').textContent, /calendar.list/);
  p.updateStatus({ phase: 'succeeded', result: '<script>bad()</script>' }); await p.poll();
  assert.equal(p.byId('agent-result').textContent, '<script>bad()</script>'); assert.equal(p.byId('agent-result').querySelector('script'), null); assert.equal(p.byId('agent-cancel').hidden, true);
  p.byId('agent-dismiss').click(); await p.poll(); assert.equal(p.byId('agent-activity').hidden, true);
  p.updateStatus({ phase: 'failed', activation_id: 2, error: 'Local Whisper is unavailable' }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, false); assert.equal(p.byId('agent-activity-error').textContent, 'Local Whisper is unavailable'); assert.equal(p.byId('agent-feedback-details').open, false);
  p.updateStatus({ phase: 'deciding', activation_id: 3, error: null }); await p.poll(); p.byId('agent-cancel').click();
  await settle(() => p.calls.some(c => c.path.endsWith('/cancel')));
});

test('startup service failures remain contextual in Commands across Settings navigation and language changes', async t => {
  const error = 'Whisper endpoint is not a verified whisper.cpp server; no microphone audio was sent <img src=x>';
  const p = await page(t, { language: 'en', status: { phase: 'failed', error, activation_id: 0 } });
  p.window.eval(fs.readFileSync(path.join(__dirname, 'workspace.js'), 'utf8'));
  assert.equal(p.byId('agent-activity').hidden, true);
  assert.equal(p.byId('agent-summary').textContent, 'Local service unavailable');
  const issue = p.byId('agent-service-status');
  assert.equal(issue.hidden, false);
  assert.equal(issue.closest('[data-workspace-panel]').dataset.workspacePanel, 'commands');
  assert.equal(p.byId('agent-service-status-detail').textContent, error);
  assert.equal(issue.querySelector('img'), null);
  assert.equal(p.byId('agent-error').hidden, true, 'service status does not replace action errors');
  p.byId('nav-settings').click(); await p.poll();
  assert.equal(p.window.BabelWorkspace.current, 'settings');
  assert.equal(p.byId('agent-activity').hidden, true);
  assert.equal(issue.closest('[data-workspace-panel]').hidden, true);
  p.byId('nav-commands').click();
  assert.equal(issue.hidden, false);
  const endpoint = p.byId('agent-whisper_endpoint'); endpoint.value = 'http://127.0.0.1:9090/inference';
  endpoint.dispatchEvent(new p.window.Event('input', { bubbles: true }));
  await p.window.BabelI18n.setLanguage('pt'); await p.poll();
  assert.equal(p.byId('agent-summary').textContent, 'Serviço local indisponível');
  assert.equal(p.byId('agent-service-status-title').textContent, 'Serviço local indisponível');
  assert.equal(p.byId('agent-service-configure').textContent, 'Revisar serviços locais');
  assert.equal(p.byId('agent-activity').hidden, true);
  p.byId('agent-service-configure').click();
  assert.equal(p.byId('agent-service-fields').closest('details').open, true);
  assert.equal(p.doc.activeElement, endpoint);
  assert.equal(endpoint.value, 'http://127.0.0.1:9090/inference');
  assert.equal(p.byId('agent-save').disabled, false);
  assert.equal(p.calls.some(c => c.request.method !== 'GET'), false, 'reviewing services must not save or invoke inference');
  p.byId('nav-settings').click(); p.byId('nav-commands').click();
  assert.equal(p.byId('agent-service-status'), issue);
  assert.equal(p.byId('agent-activity').hidden, true);
  p.updateStatus({ phase: 'listening', error: null }); await p.poll();
  assert.equal(issue.hidden, true);
  assert.equal(p.byId('agent-service-status-detail').textContent, '');
  assert.equal(p.byId('agent-summary').textContent, 'Aguardando nome de ativação');
  assert.equal(endpoint.value, 'http://127.0.0.1:9090/inference');
});

test('explicit service scope stays contextual after previous activations and preserves dismissal of real commands', async t => {
  const p = await page(t);
  p.updateStatus({ phase: 'failed', activation_id: 0, error_scope: 'command', error: 'Cancelled before activation' }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true, 'an API action without activation must not create a command overlay');
  assert.equal(p.byId('agent-service-status'), null, 'explicit command scope is not a local service diagnostic');
  p.updateStatus({ phase: 'failed', activation_id: 7, error_scope: 'command', error: 'Tool refused the command' }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, false);
  p.byId('agent-dismiss').click();
  p.updateStatus({ error_scope: 'service', error: 'Whisper unavailable before the next wake name' }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
  assert.equal(p.byId('agent-service-status').hidden, false);
  assert.equal(p.byId('agent-summary').textContent, 'Serviço local indisponível');
  // Scope alone is part of the render signature; a dismissed activation stays dismissed.
  p.updateStatus({ error_scope: 'command' }); await p.poll();
  assert.equal(p.byId('agent-service-status').hidden, true);
  assert.equal(p.byId('agent-activity').hidden, true);
  await p.window.BabelI18n.setLanguage('en'); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
  p.updateStatus({ activation_id: 8, error: 'New real command failed' }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, false);
  assert.equal(p.byId('agent-activity-title').textContent, 'Command failed');
  assert.equal(p.byId('agent-activity-error').textContent, 'New real command failed');
  p.updateStatus({ error_scope: 'service' }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
  assert.equal(p.byId('agent-service-status').hidden, false);
  p.updateStatus({ phase: 'listening', error_scope: null, error: null }); await p.poll();
  assert.equal(p.byId('agent-service-status').hidden, true);
  assert.equal(p.byId('agent-activity').hidden, true);
});

test('changing interface language preserves agent drafts, focus and secrets', async t => {
  const p = await page(t);
  p.set('agent-wake_name', 'Aurora'); p.set('mcp-test-server-name', 'Agenda pessoal');
  const input = p.byId('agent-credential-value'); input.value = 'unsavedsecret'; input.focus();
  await p.window.BabelI18n.setLanguage('en');
  assert.equal(p.byId('agent-title').textContent, 'Voice commands'); assert.equal(p.byId('agent-summary').textContent, 'Listening for wake name');
  assert.equal(p.byId('agent-wake_name').value, 'Aurora'); assert.equal(p.byId('mcp-test-server-name').value, 'Agenda pessoal');
  assert.equal(p.doc.activeElement, input); assert.equal(input.value, 'unsavedsecret'); assert.equal(p.byId('agent-save-state').textContent, 'Unsaved agent settings');
  assert.equal(p.calls.some(c => c.request.method === 'PUT'), false);
});

test('reapplying the catalog never restores the initial loading label over a real agent phase', async t => {
  const p = await page(t);
  assert.equal(p.byId('agent-summary').textContent, 'Aguardando nome de ativação');
  p.window.BabelI18n.apply();
  assert.equal(p.byId('agent-summary').textContent, 'Aguardando nome de ativação');
  p.updateStatus({ phase: 'failed', error: 'Local Whisper is unavailable', activation_id: 0 });
  await p.poll();
  assert.equal(p.byId('agent-summary').textContent, 'Serviço local indisponível');
  p.window.BabelI18n.apply();
  assert.equal(p.byId('agent-summary').textContent, 'Serviço local indisponível');
  await p.window.BabelI18n.setLanguage('pt');
  await p.poll();
  assert.equal(p.byId('agent-summary').textContent, 'Serviço local indisponível');
  assert.equal(p.byId('agent-summary').hasAttribute('data-i18n'), false);
});

test('agent revision conflicts do not overwrite drafts or flag audio settings', async t => {
  const p = await page(t);
  p.set('agent-wake_name', 'My draft'); p.externalChange(config => { config.wake_name = 'Another dashboard'; });
  p.byId('agent-form').dispatchEvent(new p.window.Event('submit', { bubbles: true, cancelable: true }));
  await settle(() => !p.byId('agent-conflict').hidden);
  assert.equal(p.byId('agent-wake_name').value, 'My draft'); assert.equal(p.config().wake_name, 'Another dashboard'); assert.equal(p.byId('config-conflict').hidden, true);
  p.byId('agent-reload').click(); await settle(() => p.byId('agent-wake_name').value === 'Another dashboard');
  assert.equal(p.byId('agent-conflict').hidden, true);
});

test('agent locales include every static field and phase in both catalogs', () => {
  const en = JSON.parse(fs.readFileSync(path.join(__dirname, 'locales/en.json'), 'utf8')); const pt = JSON.parse(fs.readFileSync(path.join(__dirname, 'locales/pt.json'), 'utf8'));
  const code = fs.readFileSync(path.join(__dirname, 'agent.js'), 'utf8');
  for (const match of code.matchAll(/['"]((?:agent|mcp)\.[a-z_]+)['"]/g)) { assert.equal(typeof en[match[1]], 'string', match[1]); assert.equal(typeof pt[match[1]], 'string', match[1]); }
  for (const phase of ['disabled', 'inactive', 'listening', 'activated', 'transcribing', 'deciding', 'executing', 'processing', 'succeeded', 'failed']) assert.equal(typeof en[`agent.phase_${phase}`], 'string');
});


test('command model and resource limits save independently and explain the idle lifecycle in both languages', async t => {
  for (const language of ['en', 'pt']) {
    const p = await page(t, { language });
    assert.equal(p.byId('agent-whisper_model').value, 'base-q5_1');
    assert.equal(p.byId('agent-idle_unload_secs').value, '60');
    p.set('agent-idle_unload_secs', 0);
    assert.equal(p.byId('agent-idle_unload_secs').checkValidity(), false);
    p.set('agent-idle_unload_secs', 120);
    p.set('agent-local_threads', 3);
    p.set('agent-whisper_model', 'tiny-q5_1');
    await p.save();
    assert.equal(p.config().whisper_model, 'tiny-q5_1');
    assert.equal(p.config().local_threads, 3);
    assert.equal(p.config().idle_unload_secs, 120);
    assert.match(p.byId('agent-idle_unload_secs-hint').textContent, /Needle/);
    assert.equal(p.calls.some(call => ['/api/config', '/api/start', '/api/stop'].includes(call.path)), false);
  }
});


test('command feedback uses the Babel mark, stays private and never takes keyboard focus', async t => {
  const p = await page(t, { language: 'en' });
  p.byId('agent-wake_name').focus();
  p.updateStatus({ phase: 'activated', activation_id: 1 }); await p.poll();
  assert.equal(p.byId('agent-activity').querySelector('img').getAttribute('src'), '/brand.svg');
  assert.equal(p.doc.activeElement, p.byId('agent-wake_name'));
  assert.equal(p.byId('agent-activity').getAttribute('aria-busy'), 'true');
  p.updateStatus({ phase: 'succeeded', command: 'Private appointment', tool: 'calendar.list', result: 'Private result' }); await p.poll();
  assert.equal(p.byId('agent-feedback-details').hidden, false);
  assert.equal(p.byId('agent-feedback-details').open, false);
  assert.equal(p.byId('agent-activity').getAttribute('aria-busy'), 'false');
  assert.match(p.byId('agent-live').textContent, /Command completed/);
  assert.doesNotMatch(p.byId('agent-live').textContent, /Private|calendar/);
  assert.equal(p.byId('agent-live').getAttribute('aria-live'), 'polite');
  assert.equal(p.byId('agent-live').getAttribute('aria-atomic'), 'true');
});

test('completed feedback remains visible after listening resumes and expires without repeated polling extending it', async t => {
  const p = await page(t, { language: 'en' });
  p.updateStatus({ phase: 'succeeded', activation_id: 1 }); await p.poll();
  assert.equal([...p.timers.values()][0].delay, 5000);
  const timer = [...p.timers.keys()][0];
  p.updateStatus({ phase: 'listening' }); await p.poll(); await p.poll();
  assert.equal(p.byId('agent-summary').textContent, 'Listening for wake name');
  assert.equal(p.byId('agent-activity-title').textContent, 'Command completed');
  assert.equal([...p.timers.keys()][0], timer);
  p.expire(); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
  p.updateStatus({ phase: 'activated', activation_id: 2 }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, false);
  assert.equal(p.timers.size, 0);
});

test('new activation cancels an older completion timer and dismissal lasts until another activation', async t => {
  const p = await page(t);
  p.updateStatus({ phase: 'failed', activation_id: 1, error: 'Rejected' }); await p.poll();
  assert.equal([...p.timers.values()][0].delay, 9000);
  const oldTimer = [...p.timers.values()][0].callback;
  p.updateStatus({ phase: 'activated', activation_id: 2, error: null }); await p.poll();
  assert.equal(p.timers.size, 0); oldTimer();
  assert.equal(p.byId('agent-activity').hidden, false);
  p.byId('agent-dismiss').click();
  p.updateStatus({ phase: 'succeeded' }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
  p.updateStatus({ phase: 'activated', activation_id: 3 }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, false);
  p.byId('agent-dismiss').dispatchEvent(new p.window.KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  assert.equal(p.byId('agent-activity').hidden, true);
});

test('hover, keyboard focus and reading details pause automatic dismissal', async t => {
  const p = await page(t);
  p.updateStatus({ phase: 'succeeded', activation_id: 1, result: 'A response' }); await p.poll();
  p.byId('agent-activity').dispatchEvent(new p.window.MouseEvent('mouseenter'));
  assert.equal(p.timers.size, 0);
  p.byId('agent-activity').dispatchEvent(new p.window.MouseEvent('mouseleave'));
  assert.equal(p.timers.size, 1);
  p.byId('agent-feedback-details').open = true;
  p.byId('agent-feedback-details').dispatchEvent(new p.window.Event('toggle'));
  assert.equal(p.timers.size, 0);
  p.byId('agent-feedback-details').open = false;
  p.byId('agent-feedback-details').dispatchEvent(new p.window.Event('toggle'));
  assert.equal(p.timers.size, 1);
  p.byId('agent-dismiss').focus(); assert.equal(p.timers.size, 0);
  p.byId('agent-wake_name').focus(); await Promise.resolve();
  assert.equal(p.timers.size, 1);
  p.expire(); assert.equal(p.byId('agent-activity').hidden, true);
});

test('preserved backend feedback shows fast completion once while opening settings never replays old commands', async t => {
  const p = await page(t, { language: 'en', status: { activation_id: 8, feedback: { activation_id: 8, sequence: 20, phase: 'succeeded', age_ms: 1000 } } });
  assert.equal(p.byId('agent-activity').hidden, true);
  p.updateStatus({ feedback: { activation_id: 8, sequence: 20, phase: 'succeeded', age_ms: 1200 } }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
  // The entire next command finished between two status requests.
  p.updateStatus({ activation_id: 9, feedback: { activation_id: 9, sequence: 23, phase: 'succeeded', age_ms: 250 } }); await p.poll();
  assert.equal(p.byId('agent-activity-title').textContent, 'Command completed');
  assert.equal(p.byId('agent-summary').textContent, 'Listening for wake name');
  assert.equal([...p.timers.values()][0].delay, 4750);
  p.expire();
  p.updateStatus({ command: 'Late status detail', feedback: { activation_id: 9, sequence: 23, phase: 'succeeded', age_ms: 1000 } }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
  p.updateStatus({ activation_id: 10, feedback: { activation_id: 10, sequence: 24, phase: 'processing', age_ms: 0 } }); await p.poll();
  assert.equal(p.byId('agent-activity-title').textContent, 'Working on your command');
  p.updateStatus({ feedback: { activation_id: 10, sequence: 25, phase: 'dismissed', age_ms: 0 } }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
});

test('expired backend feedback is ignored while an in-progress command can appear on first connection', async t => {
  const p = await page(t, { status: { phase: 'deciding', activation_id: 1, feedback: { activation_id: 1, sequence: 2, phase: 'processing', age_ms: 400 } } });
  assert.equal(p.byId('agent-activity').hidden, false);
  p.updateStatus({ phase: 'listening', activation_id: 2, feedback: { activation_id: 2, sequence: 5, phase: 'failed', age_ms: 10000 } }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, true);
  const css = fs.readFileSync(path.join(__dirname, 'style.css'), 'utf8');
  assert.match(css, /prefers-reduced-motion: reduce[\s\S]*?agent-activity[\s\S]*?animation: none/);
});


test('a first command completing between polls appears after an initially empty feedback state', async t => {
  const p = await page(t, { language: 'en', status: { feedback: null } });
  p.updateStatus({ activation_id: 1, feedback: { activation_id: 1, sequence: 3, phase: 'succeeded', age_ms: 50 } }); await p.poll();
  assert.equal(p.byId('agent-activity').hidden, false);
  assert.equal(p.byId('agent-activity-title').textContent, 'Command completed');
});
