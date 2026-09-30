'use strict';

// Agent configuration has its own revision and never writes session/audio settings.
(() => {
  const byId = id => document.getElementById(id);
  const i18n = window.BabelI18n;
  const t = (key, values) => i18n.t(key, values);
  const state = { config: null, revision: null, draft: null, dirty: false, busy: false, conflict: false, status: null, dismissed: null, polling: false, pollCount: 0, authenticated: true, auth: new Map(), tools: new Map(), renderedStatus: null, connectionError: null, feedback: null, feedbackSequence: null, feedbackInitialized: false, feedbackTimer: null, feedbackHover: false };
  const activePhases = new Set(['activated', 'transcribing', 'deciding', 'executing', 'processing']);
  const visiblePhases = new Set([...activePhases, 'succeeded', 'failed']);
  const basicFields = [
    ['wake_name', 'agent.wake_name', 'text', { maxlength: 60, required: true }], ['desktop_notifications', 'agent.desktop_notifications', 'checkbox'],
  ];
  const serviceFields = [
    ['services_directory', 'agent.services_directory', 'text', { maxlength: 4096, autocomplete: 'off', spellcheck: 'false' }],
    ['whisper_endpoint', 'agent.whisper_endpoint', 'text', { required: true, maxlength: 2048, placeholder: 'auto', autocomplete: 'off' }],
    ['whisper_model', 'local.whisper_model', [['base-q5_1', 'local.model_base_q5_1'], ['tiny-q5_1', 'local.model_tiny_q5_1'], ['small-q5_1', 'local.model_small_q5_1'], ['base', 'local.model_base'], ['tiny', 'local.model_tiny'], ['small', 'local.model_small']]],
    ['local_threads', 'agent.local_threads', 'number', { min: 1, max: 32, required: true }],
    ['idle_unload_secs', 'local.idle_unload', 'number', { min: 1, max: 3600, required: true }],
    ['whisper_language', 'agent.whisper_language', 'text', { maxlength: 12 }],
    ['whisper_api_key_env', 'agent.whisper_key', 'text', { maxlength: 128 }],
    ['needle_endpoint', 'agent.needle_endpoint', 'text', { required: true, maxlength: 2048, placeholder: 'auto', autocomplete: 'off' }],
    ['needle_api_key_env', 'agent.needle_key', 'text', { maxlength: 128 }],
    ['min_confidence', 'agent.min_confidence', 'number', { min: 0, max: 1, step: 0.01 }],
  ];
  const timingFields = [
    ['silence_ms', 'agent.silence_ms', 'number', { min: 200, max: 2000, step: 50 }],
    ['max_utterance_ms', 'agent.max_utterance_ms', 'number', { min: 1000, max: 15000, step: 500 }],
    ['command_window_secs', 'agent.command_window_secs', 'number', { min: 2, max: 30 }],
    ['timeout_secs', 'agent.timeout_secs', 'number', { min: 1, max: 120 }],
    ['max_calls', 'agent.max_calls', 'number', { min: 1, max: 8 }],
    ['vad_threshold', 'agent.vad_threshold', 'number', { min: 0.0001, max: 0.5, step: 0.0001 }],
  ];

  function element(tag, className, text) {
    const item = document.createElement(tag);
    if (className) item.className = className;
    if (text != null) item.textContent = text;
    return item;
  }
  function localized(tag, key, className) {
    const item = element(tag, className, t(key)); item.dataset.i18n = key; return item;
  }
  function message(kind, value) {
    const node = byId(`agent-${kind}`); i18n.message(node, value); node.hidden = !value;
  }
  function authHeader() {
    if (window.BabelDashboard) return window.BabelDashboard.authorization();
    try { return `Bearer ${sessionStorage.getItem('babel-token') || ''}`; } catch (_) { return 'Bearer '; }
  }
  async function request(path, { method = 'GET', body, revision, snapshot = false, timeout = 40000 } = {}) {
    const response = await fetch(`/api${path}`, {
      method, headers: { Authorization: authHeader(), ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}), ...(revision != null ? { 'If-Match': `"${revision}"` } : {}) },
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}), cache: 'no-store', credentials: 'omit', signal: AbortSignal.timeout(timeout),
    });
    const data = (response.headers.get('content-type') || '').includes('application/json') ? await response.json() : { error: await response.text() };
    if (!response.ok) {
      if (response.status === 412) { state.conflict = true; controls(); }
      if (response.status === 401 || response.status === 403) { state.authenticated = false; controls(); }
      throw new Error(data.error || t('error.http', { status: response.status }));
    }
    if (snapshot) {
      const etag = response.headers.get('etag')?.match(/^"(\d+)"$/)?.[1];
      if (etag === undefined) throw new Error(t('agent.revision_missing'));
      return { data, revision: etag };
    }
    return data;
  }
  function markDirty() { state.dirty = true; message('notice', ''); controls(); }
  function controls() {
    byId('agent-fields').disabled = !state.config || state.busy || !state.authenticated;
    byId('agent-enabled').disabled = byId('agent-fields').disabled;
    byId('agent-save').disabled = !state.config || !state.dirty || state.busy || state.conflict || !state.authenticated;
    byId('agent-save-state').textContent = t(state.dirty ? 'agent.unsaved_state' : 'agent.saved_state');
    byId('agent-save-state').classList.toggle('dirty', state.dirty);
    byId('agent-conflict').hidden = !state.conflict;
    byId('agent-reload').disabled = state.busy;
    for (const button of document.querySelectorAll('[data-mcp-network]')) button.disabled = state.dirty || state.busy || !state.authenticated || state.conflict;
    for (const input of document.querySelectorAll('[data-agent-credential]')) input.disabled = !state.config || state.busy || !state.authenticated;
    byId('agent-cancel').disabled = state.busy || !state.authenticated;
  }
  function createField(container, [key, labelKey, type, attributes = {}], getter, setter, prefix = 'agent') {
    const label = element('label', type === 'checkbox' ? 'checkbox-label' : '');
    const caption = localized('span', labelKey);
    let input;
    if (Array.isArray(type)) {
      input = element('select');
      for (const [value, optionKey] of type) { const option = localized('option', optionKey); option.value = value; input.append(option); }
    } else input = element(type === 'textarea' ? 'textarea' : 'input');
    if (type !== 'textarea' && !Array.isArray(type)) input.type = type;
    input.id = `${prefix}-${key}`;
    input.dataset.agentField = key;
    for (const [name, value] of Object.entries(attributes)) if (value !== false) input.setAttribute(name, value === true ? '' : value);
    if (type === 'textarea') input.rows = 3;
    const value = getter(key);
    if (type === 'checkbox') input.checked = Boolean(value); else input.value = value == null ? '' : String(value);
    input.addEventListener('input', () => { setter(key, type === 'checkbox' ? input.checked : type === 'number' ? Number(input.value) : input.value); markDirty(); });
    input.addEventListener('change', () => { setter(key, type === 'checkbox' ? input.checked : type === 'number' ? Number(input.value) : input.value); markDirty(); });
    if (type === 'checkbox') label.append(input, caption); else label.append(caption, input);
    container.append(label); return input;
  }
  function lines(value) { return value.split(/\r?\n/).map(item => item.trim()).filter(Boolean); }
  function mapText(values) { return Object.entries(values || {}).map(([key, value]) => `${key}=${value}`).join('\n'); }
  function readMap(value, field) {
    const result = Object.create(null);
    for (const line of lines(value)) {
      const offset = line.indexOf('=');
      if (offset <= 0) throw new Error(t('mcp.invalid_map', { field }));
      const key = line.slice(0, offset).trim();
      if (!key || ['__proto__', 'constructor', 'prototype'].includes(key)) throw new Error(t('mcp.invalid_map', { field }));
      result[key] = line.slice(offset + 1);
    }
    return result;
  }
  function collect() {
    const value = structuredClone(state.draft);
    for (const integration of value.integrations) {
      const row = [...document.querySelectorAll('[data-integration-id]')].find(node => node.dataset.integrationId === integration.id);
      if (!row) continue;
      for (const key of ['env', 'headers', 'secret_env', 'secret_headers']) integration[key] = readMap(row.querySelector(`[data-agent-field="${key}"]`).value, t(`mcp.${key}`));
      integration.args = lines(row.querySelector('[data-agent-field="args"]').value);
      integration.allowed_tools = lines(row.querySelector('[data-agent-field="allowed_tools"]').value);
      integration.oauth.scopes = lines(row.querySelector('[data-agent-field="scopes"]').value);
      if (integration.transport === 'stdio') { integration.auth = 'none'; integration.headers = {}; integration.secret_headers = {}; integration.token_env = ''; integration.oauth = { client_id: '', client_secret_env: '', scopes: [] }; }
    }
    return value;
  }
  function renderFields() {
    byId('agent-enabled').checked = Boolean(state.draft.enabled);
    for (const [containerId, fields] of [['agent-primary-fields', basicFields], ['agent-service-fields', serviceFields], ['agent-timing-fields', timingFields]]) {
      const container = byId(containerId); container.replaceChildren();
      for (const field of fields) {
        const input = createField(container, field, key => state.draft[key], (key, value) => { state.draft[key] = value; });
        const key = field[0];
        if (key === 'services_directory' || key === 'whisper_api_key_env' || ['whisper_endpoint', 'needle_endpoint'].includes(key)) {
          const hint = localized('small', key === 'services_directory' ? 'agent.services_directory_hint' : key === 'whisper_api_key_env' ? 'agent.whisper_key_hint' : 'agent.endpoint_hint');
          hint.id = `agent-${key}-hint`; input.setAttribute('aria-describedby', hint.id); input.parentElement.append(hint);
        }
        if (['whisper_model', 'local_threads', 'idle_unload_secs'].includes(key)) {
          const hint = localized('small', `agent.${key}_hint`);
          hint.id = `agent-${key}-hint`; input.setAttribute('aria-describedby', hint.id); input.parentElement.append(hint);
        }
        if (['whisper_endpoint', 'needle_endpoint'].includes(key)) {
          const service = key.split('_')[0];
          const current = element('small', 'field-hint');
          const output = element('output'); output.id = `agent-${service}-effective-endpoint`;
          current.append(localized('span', `agent.${service}_effective_endpoint`), document.createTextNode(' '), output);
          input.parentElement.append(current);
        }
      }
    }
    renderEffectiveEndpoints(state.status);
  }
  function renderEffectiveEndpoints(status) {
    for (const service of ['whisper', 'needle']) {
      const output = byId(`agent-${service}-effective-endpoint`);
      if (!output) continue;
      const endpoint = status?.[`${service}_endpoint`];
      const value = typeof endpoint === 'string' && endpoint ? endpoint : t('agent.endpoint_inactive');
      if (output.textContent !== value) output.textContent = value;
    }
  }
  function apply(config, revision) {
    state.config = structuredClone(config); state.draft = structuredClone(config); state.revision = revision;
    state.draft.integrations ||= [];
    state.dirty = false; state.conflict = false;
    renderFields(); renderIntegrations(); renderCredentials(); controls();
  }
  function updateIntegrationDisplay(integration, row) {
    row.querySelector('.mcp-name').textContent = integration.name || integration.id;
    row.querySelector('.mcp-kind').textContent = integration.transport === 'http' ? 'HTTP' : 'stdio';
    row.querySelectorAll('[data-mcp-http]').forEach(node => { node.hidden = integration.transport !== 'http'; });
    row.querySelectorAll('[data-mcp-stdio]').forEach(node => { node.hidden = integration.transport !== 'stdio'; });
    row.querySelectorAll('[data-mcp-bearer]').forEach(node => { node.hidden = integration.auth !== 'bearer' || integration.transport !== 'http'; });
    row.querySelectorAll('[data-mcp-oauth]').forEach(node => { node.hidden = integration.auth !== 'oauth' || integration.transport !== 'http'; });
    row.querySelector('[data-agent-field="url"]').required = integration.transport === 'http';
    row.querySelector('[data-agent-field="command"]').required = integration.transport === 'stdio';
  }
  function renderIntegrations() {
    const container = byId('mcp-list'); container.replaceChildren();
    if (!state.draft.integrations.length) container.append(localized('p', 'mcp.empty', 'mcp-empty'));
    for (const integration of state.draft.integrations) {
      integration.oauth ||= { client_id: '', client_secret_env: '', scopes: [] };
      const row = element('details', 'mcp-item'); row.dataset.integrationId = integration.id;
      const summary = element('summary'); summary.append(element('strong', 'mcp-name'), element('span', 'mcp-kind')); row.append(summary);
      const grid = element('div', 'profile-grid');
      const fields = [
        ['enabled', 'mcp.enabled', 'checkbox'], ['name', 'mcp.name', 'text', { maxlength: 100, required: true }],
        ['transport', 'mcp.transport', [['http', 'mcp.http'], ['stdio', 'mcp.stdio']]],
        ['url', 'mcp.url', 'url'], ['command', 'mcp.command', 'text'], ['cwd', 'mcp.cwd', 'text'],
        ['args', 'mcp.args', 'textarea'], ['env', 'mcp.env', 'textarea'], ['secret_env', 'mcp.secret_env', 'textarea'],
        ['auth', 'mcp.auth', [['none', 'mcp.auth_none'], ['bearer', 'mcp.auth_bearer'], ['oauth', 'mcp.auth_oauth']]],
        ['token_env', 'mcp.token_env', 'text'], ['headers', 'mcp.headers', 'textarea'], ['secret_headers', 'mcp.secret_headers', 'textarea'],
        ['client_id', 'mcp.client_id', 'text'], ['client_secret_env', 'mcp.client_secret_env', 'text'], ['scopes', 'mcp.scopes', 'textarea'],
        ['allowed_tools', 'mcp.allowed_tools', 'textarea'], ['timeout_secs', 'mcp.timeout_secs', 'number', { min: 1, max: 300 }],
      ];
      for (const field of fields) {
        const [key] = field;
        const getter = () => ['env', 'headers', 'secret_env', 'secret_headers'].includes(key) ? mapText(integration[key]) : ['args', 'allowed_tools'].includes(key) ? (integration[key] || []).join('\n') : key === 'scopes' ? integration.oauth.scopes.join('\n') : ['client_id', 'client_secret_env'].includes(key) ? integration.oauth[key] : integration[key];
        const setter = (_, value) => {
          if (['client_id', 'client_secret_env'].includes(key)) integration.oauth[key] = value;
          else if (!['env', 'headers', 'secret_env', 'secret_headers', 'args', 'allowed_tools', 'scopes'].includes(key)) integration[key] = value;
          if (['name', 'transport', 'auth'].includes(key)) updateIntegrationDisplay(integration, row);
        };
        const input = createField(grid, field, getter, setter, `mcp-${integration.id}`);
        const label = input.parentElement;
        if (['command', 'cwd', 'args', 'env', 'secret_env'].includes(key)) label.dataset.mcpStdio = '';
        if (['url', 'auth', 'headers', 'secret_headers'].includes(key)) label.dataset.mcpHttp = '';
        if (key === 'token_env') label.dataset.mcpBearer = '';
        if (['client_id', 'client_secret_env', 'scopes'].includes(key)) label.dataset.mcpOauth = '';
        if (key === 'allowed_tools') label.append(localized('small', 'mcp.allow_hint'));
        if (['env', 'headers'].includes(key)) label.append(localized('small', 'mcp.public_hint'));
        if (['secret_env', 'secret_headers'].includes(key)) label.append(localized('small', 'mcp.secret_hint'));
        if (['token_env', 'client_secret_env'].includes(key)) label.append(localized('small', 'mcp.reference_hint'));
      }
      row.append(grid);
      const actions = element('div', 'mcp-actions');
      const test = localized('button', 'mcp.test', 'button secondary'); test.type = 'button'; test.dataset.mcpNetwork = ''; test.dataset.mcpAction = 'test';
      test.addEventListener('click', () => action(async () => {
        const response = await request('/agent/integrations/test', { method: 'POST', body: { id: integration.id }, timeout: 125000 });
        state.tools.set(integration.id, response.tools || []); renderTools(integration.id, row);
        integrationMessage(row, t('mcp.test_success', { count: i18n.number((response.tools || []).length) }));
      }, row));
      const connect = localized('button', 'mcp.connect', 'button secondary'); connect.type = 'button'; connect.dataset.mcpNetwork = ''; connect.dataset.mcpOauth = ''; connect.dataset.mcpAction = 'connect';
      connect.addEventListener('click', () => action(async () => {
        const response = await request('/agent/oauth/begin', { method: 'POST', body: { id: integration.id } });
        const url = new URL(response.authorization_url);
        if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) throw new Error(t('mcp.invalid_auth_url'));
        const link = row.querySelector('.mcp-auth-link'); link.href = url.href; link.hidden = false;
        integrationMessage(row, t('mcp.authorize_hint'));
      }, row));
      const disconnect = localized('button', 'mcp.disconnect', 'button quiet'); disconnect.type = 'button'; disconnect.dataset.mcpNetwork = ''; disconnect.dataset.mcpOauth = ''; disconnect.dataset.mcpAction = 'disconnect';
      disconnect.addEventListener('click', () => action(async () => {
        await request('/agent/oauth/disconnect', { method: 'POST', body: { id: integration.id } });
        state.auth.set(integration.id, false); row.querySelector('.mcp-auth-link').hidden = true;
        const label = row.querySelector('.mcp-auth-state'); label.dataset.i18n = 'mcp.not_connected'; label.textContent = t(label.dataset.i18n);
        integrationMessage(row, t('mcp.disconnected'));
      }, row));
      const remove = localized('button', 'mcp.remove', 'button quiet'); remove.type = 'button'; remove.dataset.mcpAction = 'remove';
      remove.addEventListener('click', () => { state.draft = collect(); state.draft.integrations = state.draft.integrations.filter(item => item.id !== integration.id); markDirty(); renderIntegrations(); });
      actions.append(test, connect, disconnect, remove); row.append(actions);
      const authState = localized('p', state.auth.get(integration.id) ? 'mcp.connected' : 'mcp.not_connected', 'mcp-auth-state field-hint'); authState.dataset.mcpOauth = ''; row.append(authState);
      const link = localized('a', 'mcp.authorize', 'mcp-auth-link'); link.target = '_blank'; link.rel = 'noopener noreferrer'; link.hidden = true; row.append(link);
      const info = element('p', 'mcp-message'); info.setAttribute('role', 'status'); row.append(info);
      const toolList = element('ul', 'mcp-tools'); toolList.hidden = true; row.append(toolList);
      container.append(row); updateIntegrationDisplay(integration, row); renderTools(integration.id, row);
    }
    controls();
  }
  function integrationMessage(row, value) { i18n.message(row.querySelector('.mcp-message'), value); }
  function renderTools(id, row) {
    const list = row.querySelector('.mcp-tools'); list.replaceChildren();
    for (const tool of state.tools.get(id) || []) list.append(element('li', '', `${tool.name}${tool.description ? ` — ${tool.description}` : ''}`));
    list.hidden = !list.childElementCount;
  }
  function references() {
    const refs = new Set(['whisper_api_key_env', 'needle_api_key_env'].map(key => state.config?.[key]).filter(Boolean));
    for (const integration of state.config?.integrations || []) {
      for (const name of [integration.token_env, integration.oauth?.client_secret_env, ...Object.values(integration.secret_headers || {}), ...Object.values(integration.secret_env || {})]) if (name) refs.add(name);
    }
    return [...refs].sort();
  }
  function renderCredentials() {
    const previousReference = byId('agent-credential-ref')?.value;
    const previousSecret = byId('agent-credential-value')?.value || '';
    byId('agent-credentials')?.remove();
    const section = element('section', 'agent-credentials'); section.id = 'agent-credentials';
    section.append(localized('h3', 'agent.credentials'), localized('p', 'agent.credentials_hint', 'field-hint'));
    const label = element('label'); label.append(localized('span', 'agent.credential_reference'));
    const select = element('select'); select.id = 'agent-credential-ref'; select.dataset.agentCredential = '';
    for (const ref of references()) select.add(new Option(ref, ref));
    if ([...select.options].some(option => option.value === previousReference)) select.value = previousReference;
    label.append(select); section.append(label);
    const row = element('div', 'credential-row'); const valueLabel = element('label'); valueLabel.append(localized('span', 'agent.credential_value'));
    const input = element('input'); input.id = 'agent-credential-value'; input.type = 'password'; input.autocomplete = 'new-password'; input.dataset.agentCredential = ''; input.maxLength = 16384; if (select.value === previousReference) input.value = previousSecret; valueLabel.append(input);
    const apply = localized('button', 'agent.credential_apply', 'button secondary'); apply.type = 'button'; apply.dataset.agentCredential = '';
    apply.addEventListener('click', () => action(async () => {
      if (!select.value || !input.value) throw new Error(t('agent.credential_missing'));
      await request('/agent/credentials', { method: 'POST', body: { api_key_env: select.value, key: input.value } }); input.value = ''; await credentialStatus(); message('notice', t('agent.credential_applied'));
    }));
    const clear = localized('button', 'agent.credential_clear', 'button quiet'); clear.type = 'button'; clear.dataset.agentCredential = '';
    clear.addEventListener('click', () => action(async () => {
      if (!select.value) return;
      await request('/agent/credentials/clear', { method: 'POST', body: { api_key_env: select.value } }); input.value = ''; await credentialStatus(); message('notice', t('agent.credential_cleared'));
    }));
    row.append(valueLabel, apply, clear); section.append(row);
    const status = element('p', 'field-hint'); status.id = 'agent-credential-status'; status.setAttribute('role', 'status'); section.append(status);
    select.addEventListener('change', () => { input.value = ''; credentialStatus().catch(error => message('error', error.message)); });
    byId('agent-form').after(section); credentialStatus().catch(error => message('error', error.message));
  }
  async function credentialStatus() {
    const name = byId('agent-credential-ref')?.value;
    if (!name) { byId('agent-credential-status').textContent = t('agent.no_references'); return; }
    const result = await request(`/credentials?${new URLSearchParams({ api_key_env: name })}`);
    if (byId('agent-credential-ref').value === name) byId('agent-credential-status').textContent = t(result.configured ? 'agent.credential_configured' : 'agent.credential_absent');
  }
  async function action(operation, row) {
    if (state.busy) return;
    state.busy = true; controls(); message('error', ''); message('notice', '');
    try { await operation(); }
    catch (error) { if (row) integrationMessage(row, error.message); else message('error', error.name === 'TimeoutError' ? t('agent.request_timeout') : error.message); }
    finally { state.busy = false; controls(); }
  }
  function renderServiceIssue(unavailable, error) {
    let notice = byId('agent-service-status');
    if (!notice && unavailable) {
      notice = element('section', 'notice error'); notice.id = 'agent-service-status';
      notice.setAttribute('role', 'status'); notice.setAttribute('aria-labelledby', 'agent-service-status-title');
      const title = localized('strong', 'agent.service_unavailable'); title.id = 'agent-service-status-title';
      const hint = localized('p', 'agent.service_setup_hint');
      const details = element('details', 'agent-advanced');
      const description = element('p', 'field-hint'); description.id = 'agent-service-status-detail';
      details.append(localized('summary', 'agent.service_details'), description);
      const actions = element('div', 'profile-help');
      const configure = localized('button', 'agent.service_configure', 'button secondary');
      configure.id = 'agent-service-configure'; configure.type = 'button';
      configure.addEventListener('click', () => {
        window.BabelWorkspace?.navigate('commands');
        const services = byId('agent-service-fields').closest('details');
        if (services) services.open = true;
        byId('agent-whisper_endpoint')?.focus({ preventScroll: true });
        services?.scrollIntoView?.({ block: 'center', behavior: 'auto' });
      });
      const guide = localized('a', 'agent.guide');
      guide.href = '/help/voice-commands'; guide.target = '_blank'; guide.rel = 'noopener';
      guide.hreflang = 'pt'; guide.title = t('help.portuguese');
      actions.append(configure, guide); notice.append(title, hint, details, actions);
      byId('agent-error').after(notice);
    }
    if (!notice) return;
    notice.hidden = !unavailable;
    byId('agent-service-status-detail').textContent = unavailable ? error || t('agent.service_setup_hint') : '';
  }
  const completionDuration = phase => phase === 'succeeded' ? 5000 : phase === 'failed' ? 9000 : 0;
  function clearFeedbackTimer() {
    if (state.feedbackTimer != null) window.clearTimeout(state.feedbackTimer);
    state.feedbackTimer = null;
  }
  function hideFeedback() {
    clearFeedbackTimer(); state.feedback = null;
    byId('agent-activity').hidden = true; byId('agent-live').textContent = '';
  }
  function dismissFeedback() {
    state.dismissed = state.feedback?.activation_id;
    hideFeedback();
  }
  function holdFeedback() {
    return state.feedbackHover || byId('agent-activity').contains(document.activeElement) || byId('agent-feedback-details').open;
  }
  function scheduleFeedbackDismiss(delay) {
    clearFeedbackTimer();
    const snapshot = state.feedback;
    if (!snapshot || !completionDuration(snapshot.phase) || holdFeedback()) return;
    state.feedbackTimer = window.setTimeout(() => {
      state.feedbackTimer = null;
      if (state.feedback?.activation_id === snapshot.activation_id && state.feedback?.phase === snapshot.phase && !holdFeedback()) dismissFeedback();
    }, delay ?? completionDuration(snapshot.phase));
  }
  function renderFeedback(status, age = 0) {
    const panel = byId('agent-activity');
    if (!(status.activation_id > 0) || !visiblePhases.has(status.phase) || state.dismissed === status.activation_id) return;
    const previous = state.feedback;
    const changed = previous?.activation_id !== status.activation_id || previous?.phase !== status.phase;
    if (previous?.activation_id !== status.activation_id) byId('agent-feedback-details').open = false;
    state.feedback = { ...status };
    panel.dataset.stage = status.phase; panel.hidden = false;
    panel.setAttribute('aria-busy', String(activePhases.has(status.phase)));
    const name = status.wake_name || state.config?.wake_name || 'Babel';
    const label = t(`agent.phase_${status.phase}`);
    const hintKey = `agent.${status.phase}_hint`;
    const hint = t(hintKey) === hintKey ? '' : t(hintKey);
    byId('agent-activity-name').textContent = name;
    byId('agent-activity-title').textContent = label;
    byId('agent-activity-detail').textContent = hint;
    byId('agent-activity-command').textContent = status.command || '';
    byId('agent-activity-command').hidden = !status.command;
    byId('agent-activity-tool').textContent = status.tool ? t('agent.tool', { tool: status.tool }) : '';
    byId('agent-activity-tool').hidden = !status.tool;
    byId('agent-activity-error').textContent = status.error || '';
    byId('agent-activity-error').hidden = !status.error;
    byId('agent-result').textContent = status.result || '';
    byId('agent-result-container').hidden = !status.result;
    byId('agent-feedback-details').hidden = !(status.command || status.tool || status.error || status.result);
    byId('agent-cancel').hidden = !activePhases.has(status.phase);
    // Announce the state, not private speech, tool arguments or responses.
    const announcement = `${name}. ${label}. ${hint}`;
    if (byId('agent-live').textContent !== announcement) byId('agent-live').textContent = announcement;
    if (changed) scheduleFeedbackDismiss(Math.max(0, completionDuration(status.phase) - age));
  }
  function updateFeedback(status, serviceFailure) {
    const first = !state.feedbackInitialized;
    state.feedbackInitialized = true;
    if (serviceFailure || ['disabled', 'inactive'].includes(status.phase)) { hideFeedback(); return; }
    const feedback = status.feedback;
    if (feedback) {
      const changed = feedback.sequence !== state.feedbackSequence;
      state.feedbackSequence = feedback.sequence;
      const terminal = completionDuration(feedback.phase);
      if (feedback.phase === 'dismissed') { hideFeedback(); return; }
      // Opening Settings must not replay a command that already completed.
      if ((first && terminal) || (changed && terminal && feedback.age_ms >= terminal)) { hideFeedback(); return; }
      if (changed || state.feedback?.activation_id === feedback.activation_id || activePhases.has(feedback.phase)) {
        const content = status.activation_id === feedback.activation_id ? status : {};
        renderFeedback({ ...content, activation_id: feedback.activation_id, phase: feedback.phase, wake_name: status.wake_name }, feedback.age_ms || 0);
      }
      return;
    }
    // Compatibility with older servers: keep a completed command visible when
    // the recognizer returns to waiting, without extending its dismissal time.
    if (visiblePhases.has(status.phase)) renderFeedback(status);
    else if (!completionDuration(state.feedback?.phase)) hideFeedback();
  }
  function renderStatus(status) {
    state.status = status;
    const signature = JSON.stringify([i18n.language, status.phase, status.activation_id, status.wake_name, status.command, status.tool, status.result, status.error, status.error_scope, status.whisper_endpoint, status.needle_endpoint, status.feedback?.sequence, status.feedback?.phase]);
    if (signature === state.renderedStatus) return;
    state.renderedStatus = signature;
    renderEffectiveEndpoints(status);
    const phase = status.phase || 'inactive';
    // An unavailable local service before activation is a setup diagnostic,
    // not a failed spoken command. Older backends identify this with ID zero.
    const serviceFailure = status.error_scope === 'service' || (phase === 'failed' && status.error_scope == null && status.activation_id === 0);
    const phaseKey = `agent.phase_${phase}`;
    const label = serviceFailure ? t('agent.service_unavailable') : t(phaseKey) === phaseKey ? phase : t(phaseKey);
    const summary = byId('agent-summary'); delete summary.dataset.i18n; summary.dataset.stage = serviceFailure ? 'unavailable' : phase; summary.textContent = label;
    renderServiceIssue(serviceFailure, status.error);
    updateFeedback(status, serviceFailure);
    if (state.feedback) renderFeedback(state.feedback);
  }
  async function syncAuth() {
    const statuses = await request('/agent/integrations/status');
    for (const item of statuses) {
      const previous = state.auth.get(item.id); state.auth.set(item.id, item.authenticated);
      const authRow = [...document.querySelectorAll('[data-integration-id]')].find(node => node.dataset.integrationId === item.id);
      if (authRow) { const label = authRow.querySelector('.mcp-auth-state'); label.dataset.i18n = item.authenticated ? 'mcp.connected' : 'mcp.not_connected'; label.textContent = t(label.dataset.i18n); }
      if (item.authenticated && previous !== true) {
        const row = [...document.querySelectorAll('[data-integration-id]')].find(node => node.dataset.integrationId === item.id);
        if (row && !row.querySelector('.mcp-auth-link').hidden) { row.querySelector('.mcp-auth-link').hidden = true; integrationMessage(row, t('mcp.connected')); }
      }
    }
  }
  async function poll() {
    if (!state.config || state.polling || !state.authenticated) return;
    state.polling = true;
    try {
      const status = await request('/agent/status', { timeout: 8000 });
      if (state.connectionError) { if (byId('agent-error').textContent === state.connectionError) message('error', ''); state.connectionError = null; state.renderedStatus = null; }
      renderStatus(status);
      state.pollCount++;
      if (state.pollCount % 5 === 0) await syncAuth();
      if ((status.config_revision != null && String(status.config_revision) !== state.revision || state.pollCount % 10 === 0) && !state.busy && !state.conflict) {
        const snapshot = await request('/agent', { snapshot: true });
        if (snapshot.revision !== state.revision) {
          if (state.dirty) { state.conflict = true; controls(); }
          else apply(snapshot.data, snapshot.revision);
        }
      }
    } catch (error) { state.connectionError = error.message; state.renderedStatus = null; delete byId('agent-summary').dataset.i18n; byId('agent-summary').textContent = t('agent.unavailable'); byId('agent-summary').dataset.stage = 'unavailable'; hideFeedback(); message('error', error.message); }
    finally { state.polling = false; }
  }
  byId('agent-enabled').addEventListener('change', event => {
    state.draft.enabled = event.target.checked; markDirty();
  });
  byId('agent-form').addEventListener('submit', event => {
    event.preventDefault();
    if (state.conflict || !state.dirty || !event.target.reportValidity()) return;
    action(async () => {
      const config = collect();
      const saved = await request('/agent', { method: 'PUT', body: config, revision: state.revision, snapshot: true });
      apply(config, saved.revision); message('notice', t('agent.saved'));
    });
  });
  byId('agent-reload').addEventListener('click', () => action(async () => { const snapshot = await request('/agent', { snapshot: true }); apply(snapshot.data, snapshot.revision); message('notice', t('agent.reloaded')); }));
  byId('mcp-add').addEventListener('click', () => {
    try {
      state.draft = collect();
      const id = `mcp-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 7)}`;
      state.draft.integrations.push({ id, name: t('mcp.new_name'), enabled: true, transport: 'http', command: '', args: [], env: {}, cwd: '', url: '', auth: 'none', token_env: '', headers: {}, secret_headers: {}, secret_env: {}, oauth: { client_id: '', client_secret_env: '', scopes: [] }, timeout_secs: 30, allowed_tools: [] });
      markDirty(); renderIntegrations();
      const rows = document.querySelectorAll('[data-integration-id]'); const row = rows[rows.length - 1]; row.open = true; row.querySelector('[data-agent-field="name"]').focus();
    } catch (error) { message('error', error.message); }
  });
  byId('agent-dismiss').addEventListener('click', dismissFeedback);
  byId('agent-activity').addEventListener('mouseenter', () => { state.feedbackHover = true; clearFeedbackTimer(); });
  byId('agent-activity').addEventListener('mouseleave', () => { state.feedbackHover = false; scheduleFeedbackDismiss(); });
  byId('agent-activity').addEventListener('focusin', clearFeedbackTimer);
  byId('agent-activity').addEventListener('focusout', () => queueMicrotask(() => scheduleFeedbackDismiss()));
  byId('agent-feedback-details').addEventListener('toggle', () => scheduleFeedbackDismiss());
  byId('agent-activity').addEventListener('keydown', event => { if (event.key === 'Escape') { event.preventDefault(); dismissFeedback(); } });
  byId('agent-cancel').addEventListener('click', () => action(async () => { await request('/agent/cancel', { method: 'POST', body: {} }); await poll(); }));
  window.addEventListener('babel:languagechange', () => {
    controls(); if (state.status) renderStatus(state.status);
    for (const link of byId('voice-agent').querySelectorAll('a[href^="/help/"]')) { link.hreflang = 'pt'; link.title = t('help.portuguese'); }
  });
  async function initialize() {
    try {
      await i18n.load('en');
      const snapshot = await request('/agent', { snapshot: true }); apply(snapshot.data, snapshot.revision);
      i18n.apply(byId('voice-agent')); await poll(); await syncAuth();
    } catch (error) { message('error', error.message); delete byId('agent-summary').dataset.i18n; byId('agent-summary').textContent = t('agent.unavailable'); }
  }
  initialize(); setInterval(poll, 1000);
})();
