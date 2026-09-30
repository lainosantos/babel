'use strict';

(() => {
  const byId = (id) => document.getElementById(id);
  const routeNames = ['microphone', 'speaker'];
  const form = byId('configuration');
  const state = { config: null, configRevision: null, configConflict: false, syncing: false, activity: 0, devices: [], status: null, statusFresh: false, autostart: null, platform: null, platformError: null, interface: null, busy: false, dirty: false, authenticated: false, polling: false, starting: false, cancelStarting: false, startCancelled: false };
  const i18n = window.BabelI18n;
  const t = (key, values) => i18n.t(key, values);
  const filePathPreview = { revision: 0, timer: null, controller: null, phase: 'idle', paths: null, error: '' };
  let translationSelection = [...routeNames];
  let token = new URLSearchParams(location.hash.slice(1)).get('token');
  try {
    if (token) sessionStorage.setItem('babel-token', token);
    else token = sessionStorage.getItem('babel-token');
  } catch (_) { /* The in-memory capability still works when browser storage is disabled. */ }
  if (location.hash) history.replaceState(null, '', location.pathname + location.search);

  window.BabelDashboard = Object.freeze({ authorization: () => `Bearer ${token || ''}` });

  function announce(message) {
    i18n.message(byId('notice'), message);
    byId('notice').hidden = !message;
  }

  function showError(message) {
    i18n.message(byId('error'), message);
    byId('error').hidden = !message;
  }

  async function api(path, options = {}) {
    const response = await fetch(`/api${path}`, {
      method: options.method || 'GET',
      headers: { Authorization: `Bearer ${token || ''}`, ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...(options.revision != null ? { 'If-Match': `"${options.revision}"` } : {}) },
      body: options.body ? JSON.stringify(options.body) : undefined,
      cache: 'no-store',
      credentials: 'omit',
      signal: options.signal || AbortSignal.timeout(options.timeout || 35000),
    });
    const contentType = response.headers.get('content-type') || '';
    const body = contentType.includes('application/json') ? await response.json() : { error: await response.text() };
    if (!response.ok) {
      if (response.status === 401 || response.status === 403) {
        state.authenticated = false;
        updateControls();
      }
      if (response.status === 412) markConfigConflict();
      const error = new Error(body.error || t('error.http', { status: response.status }));
      error.status = response.status;
      throw error;
    }
    if (options.snapshot) {
      const revision = response.headers.get('etag')?.match(/^"(\d+)"$/)?.[1];
      if (revision === undefined) throw new Error(t("ui.could_not_verify_the_settings_revision_update_babel_and_reload_the_dashboar"));
      return { body, revision };
    }
    return body;
  }

  function markConfigConflict() {
    state.configConflict = true;
    byId('config-conflict').hidden = false;
    updateControls();
  }

  async function reloadConfig() {
    if (state.syncing) return;
    state.syncing = true;
    updateControls();
    try {
      const snapshot = await api('/config', { snapshot: true });
      state.configRevision = snapshot.revision;
      applyConfig(snapshot.body);
      await refreshInterface();
      state.configConflict = false;
      byId('config-conflict').hidden = true;
      refreshCredentialStatus(byId('profile-selector').value);
      refreshCredentialStatus(byId('stt-profile-selector').value, true);
    } catch (error) { markConfigConflict(); throw error; }
    finally { state.syncing = false; updateControls(); }
  }

  async function synchronizeConfig(status) {
    if (state.configRevision === null || String(status.config_revision) === state.configRevision || state.configConflict) return;
    if (state.dirty) { markConfigConflict(); return; }
    await reloadConfig();
    announce(t("ui.settings_updated_from_the_tray_or_another_dashboard"));
  }

  function readValue(element) {
    if (element.type === 'checkbox') return element.checked;
    if (element.dataset.minutes !== undefined) return Math.round(Number(element.value) * 60);
    if (element.type === 'number' || element.type === 'range') return Number(element.value);
    return element.value;
  }

  function writeValue(element, value) {
    if (element.type === 'checkbox') element.checked = Boolean(value);
    else if (element.dataset.minutes !== undefined) element.value = value == null ? '' : String(value / 60);
    else element.value = value == null ? '' : String(value);
  }

  function fieldContainer(config, element) {
    if (element.dataset.recordingMix !== undefined) return config.recording?.mix;
    if (element.dataset.transcriptionProfile) return config.transcription?.providers?.[element.dataset.transcriptionProfile];
    if (element.dataset.recognitionRoute) return config.transcription?.[`${element.dataset.recognitionRoute}_recognition`];
    if (element.dataset.profile) return config.providers?.[element.dataset.profile];
    return config[element.dataset.route || element.dataset.section];
  }

  function managedEndpoint(id) { return byId(id).value === 'auto'; }

  function syncLocalModes() {
    document.querySelectorAll('[data-managed-mode]').forEach(select => {
      const endpoint = byId(select.dataset.managedMode);
      select.value = endpoint.value === 'auto' ? 'auto' : 'external';
      delete endpoint.dataset.externalDraft;
    });
    updateLocalControls();
  }

  function updateLocalControls() {
    document.querySelectorAll('[data-managed-mode]').forEach(select => {
      const endpoint = byId(select.dataset.managedMode);
      const managed = managedEndpoint(endpoint.id);
      document.querySelector(`[data-external-endpoint="${endpoint.id}"]`).hidden = managed;
      endpoint.disabled = managed;
    });
    const managedWhisper = managedEndpoint('stt-profile-whisper-endpoint');
    document.querySelectorAll('[data-whisper-external]').forEach(group => {
      group.hidden = managedWhisper;
      group.querySelectorAll('input, button').forEach(control => { control.disabled = managedWhisper; });
    });
    byId('stt-profile-whisper-model').disabled = !managedWhisper;
    byId('profile-local-whisper_model').disabled = !managedEndpoint('profile-local-whisper_endpoint');
    byId('profile-local-translation_model').readOnly = managedEndpoint('profile-local-ollama_endpoint');
    byId('profile-local-translation_api').disabled = managedEndpoint('profile-local-ollama_endpoint');
    byId('profile-local-translation_api').closest('label').hidden = managedEndpoint('profile-local-ollama_endpoint');
    validateLocalDirectory();
    renderLocalRuntime();
  }

  function validateLocalDirectory() {
    const field = byId('local-runtime-directory');
    const value = field.value;
    const os = platformOs();
    const windowsAbsolute = /^[a-z]:[\\/]/i.test(value)
      || /^\\\\\?\\[a-z]:\\/i.test(value)
      || /^\\\\\?\\UNC\\[^\\]+\\[^\\]+(?:\\|$)/i.test(value)
      || /^[\\/]{2}(?![?.](?:[\\/]|$))[^\\/]+[\\/][^\\/]+(?:[\\/]|$)/.test(value);
    const absolute = os === 'windows' ? windowsAbsolute : os === 'unknown' ? windowsAbsolute || value.startsWith('/') : value.startsWith('/');
    const error = value && (!absolute || value.includes('\0'))
      ? t(os === 'windows' ? 'files.base_absolute_windows' : os === 'unknown' ? 'files.base_absolute_unknown' : 'files.base_absolute_unix') : '';
    field.setCustomValidity(error);
    field.setAttribute('aria-invalid', String(Boolean(error)));
    byId('local-runtime-directory-error').textContent = error;
    byId('local-runtime-directory-error').hidden = !error;
  }

  function renderLocalRuntime() {
    const runtime = state.status?.local_runtime;
    const phase = ['idle', 'cached', 'preparing', 'ready', 'error'].includes(runtime?.phase) ? runtime.phase : 'idle';
    const selected = {
      translation: routeNames.some(route => routeProvider(route) === 'local') && ['whisper_endpoint', 'ollama_endpoint', 'piper_endpoint'].some(field => managedEndpoint(`profile-local-${field}`)),
      transcription: routeNames.some(route => sttProvider(route) === 'whisper') && managedEndpoint('stt-profile-whisper-endpoint'),
      settings: true,
    };
    for (const box of document.querySelectorAll('[data-local-runtime]')) {
      box.hidden = !selected[box.dataset.localRuntime];
      box.dataset.phase = phase;
      box.setAttribute('aria-busy', String(phase === 'preparing'));
      const setText = (selector, content) => { const node = box.querySelector(selector); if (node.textContent !== content) node.textContent = content; };
      setText('[data-local-runtime-label]', t(`local.${phase}`));
      setText('[data-local-runtime-summary]', t(`local.${phase}_hint`));
      const problem = box.querySelector('[data-local-runtime-error]');
      const detail = phase === 'error' && typeof runtime?.message === 'string' ? runtime.message : '';
      if (problem.textContent !== detail) problem.textContent = detail;
      problem.hidden = !detail;
      const download = runtime?.download;
      const showDownload = phase === 'preparing' && download && Number.isFinite(download.received) && download.received >= 0;
      box.querySelector('[data-local-runtime-download]').hidden = !showDownload;
      const progress = box.querySelector('progress');
      if (showDownload) {
        const total = Number.isFinite(download.total) && download.total > 0 ? download.total : null;
        const received = Math.min(download.received, total || Infinity);
        const percent = total ? Math.round(received / total * 100) : null;
        if (total) progress.value = received / total; else progress.removeAttribute('value');
        setText('[data-local-runtime-download-label]', t(total ? 'local.download_known' : 'local.download_unknown', {
          name: typeof download.name === 'string' ? download.name : 'Model',
          received: i18n.number(received / 1048576, { maximumFractionDigits: 1 }),
          total: total ? i18n.number(total / 1048576, { maximumFractionDigits: 1 }) : '',
          percent: percent == null ? '' : i18n.number(percent),
        }));
      }
    }
  }

  document.querySelectorAll('[data-managed-mode]').forEach(select => select.addEventListener('input', () => {
    const endpoint = byId(select.dataset.managedMode);
    if (select.value === 'auto') {
      if (endpoint.value !== 'auto') endpoint.dataset.externalDraft = endpoint.value;
      endpoint.value = 'auto';
      if (endpoint.id === 'profile-local-ollama_endpoint') {
        const model = byId('profile-local-translation_model');
        model.dataset.externalDraft = model.value;
        model.value = 'qwen3-0.6b';
      }
    } else {
      endpoint.value = endpoint.dataset.externalDraft || '';
      if (endpoint.id === 'profile-local-ollama_endpoint') {
        const model = byId('profile-local-translation_model');
        model.value = model.dataset.externalDraft || model.value;
      }
    }
    updateLocalControls();
    endpoint.dispatchEvent(new Event('input', { bubbles: true }));
  }));

  function routeProvider(route) { return byId(`${route}-provider`).value; }
  function profileValue(provider, field) { return byId(`profile-${provider}-${field}`)?.value || ''; }
  function sttProfileValue(provider, field) { return byId(`stt-profile-${provider}-${field}`)?.value || ''; }
  function sttProvider(route) { return byId(`stt-${route}-provider`).value; }
  function transcribesRoute(route) { return byId('transcription-enabled').checked && byId(`transcription-${route}`).checked; }
  function isOpenAITranslation(model) { return model === 'gpt-realtime-translate' || model.startsWith('gpt-realtime-translate-'); }
  function dedicatedRoute(route) {
    const provider = routeProvider(route);
    const model = profileValue(provider, 'model').replace(/^models\//, '');
    return (provider === 'gemini' && model === 'gemini-3.5-live-translate-preview') || (provider === 'openai' && isOpenAITranslation(model));
  }

  function collectConfig() {
    const config = structuredClone(state.config);
    document.querySelectorAll('[data-field]').forEach((element) => {
      const container = fieldContainer(config, element);
      if (!container) return;
      const field = element.dataset.field;
      if (field === 'source_language' && element.dataset.savedSource !== undefined) container[field] = element.dataset.savedSource;
      else container[field] = readValue(element);
    });
    for (const route of routeNames) {
      if (config[route].enabled && dedicatedRoute(route)) {
        config[route].prompt = '';
      }
    }
    return config;
  }

  function applyConfig(config) {
    if (!config.providers) throw new Error(t("ui.these_settings_use_an_old_format_restart_the_updated_babel_to_load_provider"));
    config.local_runtime = { directory: '', threads: 2, idle_unload_secs: 60, ...config.local_runtime };
    config.history ??= { enabled: true, duration_secs: 600 };
    config.recording.mix = { microphone_gain_db: 0, speaker_gain_db: 0, microphone_priority: true, ducking_db: 12, microphone_threshold_db: -50, ...config.recording.mix };
    state.config = config;
    writeValue(byId('files-base_path'), config.files?.base_path ?? '');
    document.querySelectorAll('[data-field]').forEach((element) => {
      const container = fieldContainer(config, element);
      delete element.dataset.savedSource;
      delete element.dataset.externalDraft;
      if (container && Object.hasOwn(container, element.dataset.field)) writeValue(element, container[element.dataset.field]);
      if (element.dataset.deviceDirection) delete element.dataset.initialized;
    });
    translationSelection = routeNames.filter(route => config[route]?.enabled);
    if (!translationSelection.length) translationSelection = [...routeNames];
    syncLocalModes();
    if (!byId('history-include').checked) byId('history-request-minutes').value = String(Math.min(600, config.history.duration_secs) / 60);
    renderDevices();
    updateProviderControls();
    updateGainLabels();
    updateQualityHint();
    state.dirty = false;
    updateControls();
    scheduleFilePathPreview(0);
  }

  function validateBasePath() {
    const field = byId('files-base_path');
    const os = platformOs();
    const value = field.value;
    // Interpret only the host's path syntax; the browser may run on another OS.
    const windowsAbsolute = /^[a-z]:[\\/]/i.test(value)
        || /^\\\\\?\\[a-z]:\\/i.test(value)
        || /^\\\\\?\\UNC\\[^\\]+\\[^\\]+(?:\\|$)/i.test(value)
        || /^[\\/]{2}(?![?.](?:[\\/]|$))[^\\/]+[\\/][^\\/]+(?:[\\/]|$)/.test(value);
    const unixAbsolute = value.startsWith('/');
    const absolute = os === 'windows' ? windowsAbsolute : os === 'unknown' ? windowsAbsolute || unixAbsolute : unixAbsolute;
    const message = !value || !absolute || value.includes('\0')
      ? t(os === 'windows' ? 'files.base_absolute_windows' : os === 'unknown' ? 'files.base_absolute_unknown' : 'files.base_absolute_unix') : '';
    field.placeholder = t(os === 'windows' ? 'files.base_placeholder_windows' : ['linux', 'macos'].includes(os) ? 'files.base_placeholder_unix' : 'files.base_placeholder');
    field.setCustomValidity(message);
    if (message) field.setAttribute('aria-invalid', 'true'); else field.removeAttribute('aria-invalid');
    const notice = byId('files-base-error');
    notice.textContent = message || (os === 'unknown' ? t('files.base_host_unavailable') : '');
    notice.hidden = !notice.textContent;
    notice.className = message ? 'file-path-error' : 'field-hint';
    notice.setAttribute('role', message ? 'alert' : 'status');
    return !message;
  }

  function renderFilePathPreview() {
    validateBasePath();
    const { phase, paths, error } = filePathPreview;
    const preview = byId('files-path-preview');
    preview.dataset.state = phase;
    preview.setAttribute('aria-busy', String(phase === 'loading'));
    for (const [id, key] of [['files-resolved-base', 'base_path'], ['transcription-resolved-directory', 'transcription_directory'], ['recording-resolved-directory', 'recording_directory']]) {
      byId(id).textContent = phase === 'ready' ? paths[key] : '—';
    }
    byId('files-path-status').textContent = t(`files.preview_${phase}`);
    byId('files-path-error').hidden = phase !== 'error';
    byId('files-path-error').textContent = phase === 'error' ? t('files.preview_error_detail', { error }) : '';
  }

  function scheduleFilePathPreview(delay = 300) {
    if (!state.authenticated || !state.config) return;
    clearTimeout(filePathPreview.timer);
    filePathPreview.controller?.abort();
    const revision = ++filePathPreview.revision;
    filePathPreview.phase = !validateBasePath() ? 'invalid' : platformOs() === 'unknown' ? 'unavailable' : 'loading'; filePathPreview.paths = null; filePathPreview.error = '';
    renderFilePathPreview();
    if (filePathPreview.phase === 'loading') filePathPreview.timer = setTimeout(() => refreshFilePathPreview(revision), delay);
  }

  async function refreshFilePathPreview(revision) {
    if (revision !== filePathPreview.revision) return;
    const controller = new AbortController();
    filePathPreview.controller = controller;
    let timedOut = false;
    const timeout = setTimeout(() => { timedOut = true; controller.abort(); }, 10000);
    const body = {
      base_path: byId('files-base_path').value,
      transcription_directory: byId('transcription-directory').value,
      recording_directory: byId('recording-directory').value,
    };
    try {
      const paths = await api('/file-paths', { method: 'POST', body, signal: controller.signal });
      if (revision !== filePathPreview.revision) return;
      if (!paths || ['base_path', 'transcription_directory', 'recording_directory'].some(key => typeof paths[key] !== 'string' || !paths[key] || paths[key].includes('\0'))) throw new Error(t('files.preview_invalid_response'));
      filePathPreview.paths = paths; filePathPreview.phase = 'ready';
    } catch (error) {
      if (revision !== filePathPreview.revision) return;
      filePathPreview.phase = error.status === 404 ? 'unsupported' : 'error';
      filePathPreview.error = timedOut ? t('files.preview_timeout') : error.message;
    } finally {
      clearTimeout(timeout);
      if (revision === filePathPreview.revision) { filePathPreview.controller = null; renderFilePathPreview(); }
    }
  }

  function renderDevices() {
    for (const select of document.querySelectorAll('[data-device-direction]')) {
      const selected = select.dataset.initialized ? select.value : state.config?.[select.dataset.route]?.[select.dataset.field] || '';
      const options = state.devices.filter((device) => device.direction === select.dataset.deviceDirection);
      select.replaceChildren(new Option(t("ui.select_a_device"), ''));
      for (const device of options) select.add(new Option(device.is_virtual ? t('device.virtual', { name: device.name }) : device.name, device.id));
      if (selected && !options.some((device) => device.id === selected)) select.add(new Option(t('device.unavailable', { name: selected }), selected));
      select.value = selected;
      select.dataset.initialized = 'true';
      for (const option of select.options) {
        const device = options.find(item => item.id === option.value);
        option.dataset.deviceName = device?.name || option.value;
        option.dataset.deviceVirtual = String(Boolean(device?.is_virtual));
        option.dataset.deviceUnavailable = String(Boolean(option.value && !device));
      }
    }
  }

  async function refreshDevices() { state.devices = await api('/devices'); renderDevices(); renderPlatformEndpoints(); }

  function platformOs() { return ['linux', 'macos', 'windows'].includes(state.platform?.os) ? state.platform.os : 'unknown'; }
  function managesVirtualDevices() { return platformOs() === 'linux' && state.platform?.manages_virtual_devices === true; }
  function supportsHostAutostart() {
    const methods = { linux: 'xdg', macos: 'launch_agent', windows: 'registry_run' };
    return Object.hasOwn(methods, platformOs()) && state.platform?.autostart_method === methods[platformOs()];
  }
  function platformText(id, key, values) {
    const node = byId(id);
    // Static i18n loading text must not overwrite host metadata on language changes.
    delete node.dataset.i18n;
    node.textContent = t(key, values);
  }
  function renderPlatformEndpoints() {
    const os = platformOs();
    for (const route of routeNames) {
      const control = byId(`${route}-${route === 'microphone' ? 'playback_device' : 'capture_device'}`);
      const device = state.devices.find(item => item.id === control.value && item.is_virtual);
      const fallback = route === 'microphone' ? 'platform.virtual_microphone' : 'platform.virtual_output';
      const node = byId(`${route}-virtual-label`); delete node.dataset.i18n;
      if (os === 'linux') node.textContent = route === 'microphone' ? 'Babel_Microphone' : 'Babel_Speaker';
      else if (os === 'macos' && device) node.textContent = device.name;
      else node.textContent = t(fallback);
    }
  }
  function renderNativeDeviceGuide(os) {
    let guide = byId('platform-native-guide');
    if (!['macos', 'windows'].includes(os)) { if (guide) guide.hidden = true; return; }
    if (!guide) {
      guide = document.createElement('details'); guide.id = 'platform-native-guide'; guide.className = 'native-device-guide';
      byId('platform-device-permissions').after(guide);
    }
    guide.hidden = false;
    if (guide.dataset.os !== os) {
      guide.dataset.os = os; guide.replaceChildren();
      const text = (tag, key) => {
        const node = document.createElement(tag); node.dataset.i18n = key; node.textContent = t(key); return node;
      };
      guide.append(text('summary', 'platform.native_mapping'));
      const scroll = document.createElement('div'); scroll.className = 'native-device-table';
      const table = document.createElement('table');
      table.append(text('caption', 'platform.native_mapping_hint'));
      const header = document.createElement('thead'); const heading = document.createElement('tr');
      for (const key of ['platform.mapping_where', 'platform.mapping_device']) { const cell = text('th', key); cell.scope = 'col'; heading.append(cell); }
      header.append(heading); table.append(header);
      const body = document.createElement('tbody');
      const endpoints = os === 'macos' ? ['Babel Microphone', 'Babel Microphone', 'Babel Speaker', 'Babel Speaker'] : ['Babel Microphone Feed', 'Babel Microphone', 'Babel Speaker', 'Babel Speaker Monitor'];
      for (const [index, key] of ['mic_capture', 'mic_playback', 'app_microphone', 'app_speaker', 'speaker_capture', 'speaker_playback'].entries()) {
        const row = document.createElement('tr'); const label = text('th', `platform.mapping_${key}`); label.scope = 'row';
        const value = index === 0 ? text('td', 'platform.mapping_physical_microphone') : index === 5 ? text('td', 'platform.mapping_physical_output') : document.createElement('td');
        if (index > 0 && index < 5) value.textContent = endpoints[index - 1];
        row.append(label, value); body.append(row);
      }
      table.append(body); scroll.append(table); guide.append(scroll);
      guide.append(text('p', `platform.${os}_driver_status`), text('p', 'platform.native_install_manual'), text('p', `platform.${os}_activity`), text('p', `platform.${os}_external`));
      const links = document.createElement('div'); links.className = 'platform-help';
      const urls = ['/help/native-drivers', '/help/platforms'];
      for (const [index, key] of ['platform.native_driver_guide', 'platform.native_routing_guide'].entries()) {
        const link = text('a', key); link.href = urls[index]; link.target = '_blank'; link.rel = 'noopener noreferrer'; links.append(link);
      }
      guide.append(links);
    }
    i18n.apply(guide);
  }
  function renderPlatform() {
    const os = platformOs();
    const names = { linux: 'Linux', macos: 'macOS', windows: 'Windows' };
    platformText('platform-device-setup', `platform.${os}_setup`);
    platformText('platform-device-details', `platform.${os}_details`);
    platformText('platform-device-permissions', `platform.${os}_permissions`);
    platformText('platform-autostart', supportsHostAutostart() ? `platform.${os}_autostart` : 'platform.autostart_unavailable');
    platformText('platform-guide', os === 'unknown' ? 'platform.current_guide' : 'platform.named_guide', { name: names[os] || '' });
    byId('platform-error').hidden = !state.platformError;
    if (state.platformError) platformText('platform-error', 'platform.read_error', { error: state.platformError });
    byId('install-devices').hidden = !managesVirtualDevices();
    byId('uninstall-devices').hidden = !managesVirtualDevices();
    byId('device-external-guide').hidden = !['macos', 'windows'].includes(os);
    if (os === 'macos' || os === 'windows') {
      platformText('device-external-guide', `platform.${os}_install`);
      byId('device-external-guide').href = '/help/native-drivers';
    }
    validateLocalDirectory();
    renderNativeDeviceGuide(os);
    renderPlatformEndpoints(); updateControls();
  }
  async function refreshPlatform() {
    try {
      const metadata = await api('/platform');
      if (!metadata || typeof metadata !== 'object' || !['linux', 'macos', 'windows', 'unknown'].includes(metadata.os)) throw new Error(t('platform.invalid_metadata'));
      state.platform = metadata; state.platformError = null;
    } catch (error) { state.platform = null; state.platformError = error.message; }
    renderPlatform();
    scheduleFilePathPreview(0);
  }

  function updateProviderControls() {
    for (const route of routeNames) {
      const translating = byId(`${route}-enabled`).checked;
      const dedicated = dedicatedRoute(route) && translating;
      const provider = routeProvider(route);
      const source = byId(`${route}-source_language`);
      source.disabled = dedicated;
      if (dedicated) { if (source.dataset.savedSource === undefined) source.dataset.savedSource = source.value; source.value = t("ui.automatic"); }
      else if (!dedicated && source.dataset.savedSource !== undefined) { source.value = source.dataset.savedSource; delete source.dataset.savedSource; }
      byId(`${route}-target_language`).disabled = !translating;
      byId(`${route}-gain`).disabled = !translating;
      byId(`${route}-prompt`).disabled = !translating || dedicated;
      byId(`${route}-prompt-hint`).textContent = dedicated
        ? t("ui.this_continuous_model_does_not_accept_prompts_saving_in_this_mode_clears_th")
        : !translating ? t("ui.translation_instructions_are_not_used_while_this_translation_is_off")
        : t("ui.add_tone_terminology_and_proper_names_to_guide_translation");
      byId(`${route}-provider-hint`).textContent = !translating
        ? t("ui.original_audio_if_transcription_is_enabled_for_this_source_this_provider_re")
        : provider === 'local' ? t("ui.transcription_translation_and_voice_through_local_services_in_segments")
        : dedicated ? t("ui.continuous_translation_with_automatic_source_language_detection") : t("ui.conversation_model_usually_waits_for_pauses_before_responding");
    }
    byId('profile-gemini-hint').textContent = t("translation.gemini_hint");
    byId('profile-openai-hint').textContent = t("translation.openai_hint");
    const local = routeNames.every(route => {
      const translates = byId(`${route}-enabled`).checked;
      return (!translates || routeProvider(route) === 'local')
        && (!transcribesRoute(route) || sttProvider(route) === 'whisper');
    });
    byId('footer-state').textContent = local ? t("ui.session_configured_for_local_processing") : t("ui.session_configured_with_cloud_providers");
    updateLocalControls();
    updateTranscriptionControls();
    renderSignalPaths();
  }

  function updateTranscriptionControls() {
    for (const route of routeNames) byId(`stt-${route}-language`).required = transcribesRoute(route);
    for (const provider of ['gemini', 'openai', 'deepgram', 'whisper']) {
      const used = routeNames.some(route => transcribesRoute(route) && sttProvider(route) === provider);
      byId(`stt-profile-${provider}-endpoint`).required = used && provider !== 'openai' && !(provider === 'whisper' && managedEndpoint('stt-profile-whisper-endpoint'));
      byId(`stt-profile-${provider}-api_key_env`).required = used && provider !== 'whisper';
      const model = byId(`stt-profile-${provider}-model`);
      if (model) model.required = used;
      for (const number of byId(`stt-profile-${provider}`).querySelectorAll('input[type="number"]')) number.required = used;
      if (provider === 'whisper') {
        const silence = byId('stt-profile-whisper-silence_ms');
        silence.setCustomValidity(used && Number(silence.value) >= Number(byId('stt-profile-whisper-segment_ms').value) ? t('stt.silence_shorter') : '');
      }
    }
  }

  function routeWaiting(route, status = state.status) {
    return status?.[route]?.state === 'waiting_for_app' && !status[route].device_error;
  }

  function routeActive(route, status = state.status) {
    const current = status?.[route];
    const inactive = ['waiting_for_app', 'unconfigured', 'stopped', 'error', 'failed', 'disabled'];
    return Boolean((status?.running || status?.routing_active) && current && !current.device_error && !inactive.includes(current.state));
  }

  function waitingHint(route) {
    return t(route === 'microphone' ? 'routing.waiting_microphone' : 'routing.waiting_speaker');
  }

  function renderSignalPaths() {
    renderPlatformEndpoints();
    for (const route of routeNames) {
      const active = routeActive(route);
      const translating = active && state.status?.running && state.config?.[route]?.enabled;
      const node = byId(`${route}-signal`);
      node.textContent = !active ? '—' : translating ? t("ui.ai") : t("ui.original");
      node.classList.toggle('original', Boolean(active && !translating));
      node.parentElement.setAttribute('aria-label', `${route === 'microphone' ? t("ui.physical_microphone_to_virtual_microphone") : t("ui.virtual_output_to_headphones")}: ${routeWaiting(route) ? waitingHint(route) : !active ? t("ui.routing_unavailable") : translating ? t("ui.ai_translation") : t("ui.original_audio_routing")}.`);
      byId(`${route}-output-label`).textContent = translating ? t("ui.translated") : t("ui.original");
    }
  }

  function updateQualityHint() {
    const descriptions = {
      low_latency: t("ui.conversation_10_ms_packets_and_200_ms_silence"),
      balanced: t("ui.conversation_20_ms_packets_and_400_ms_silence"),
      high_quality: t("ui.conversation_40_ms_packets_and_700_ms_silence"),
    };
    byId('quality-hint').textContent = t('audio.quality_hint', { description: descriptions[byId('audio-quality').value] || '' });
  }

  function updateGainLabels() {
    for (const route of routeNames) byId(`${route}-gain-value`).textContent = i18n.number(Number(byId(`${route}-gain`).value), { style: 'percent', maximumFractionDigits: 0 });
  }

  function historySeconds(field) {
    const seconds = Number(field.value) * 60;
    return field.value.trim() && Number.isFinite(seconds) && Math.abs(seconds - Math.round(seconds)) < 0.000001 ? Math.round(seconds) : NaN;
  }

  function historySelectionAvailable() {
    return byId('history-enabled').checked && state.status?.history?.enabled && routeNames.some(route =>
      ((byId('recording-enabled').checked && byId(`recording-${route}`).checked) || transcribesRoute(route))
      && Number(state.status.history[`${route}_secs`]) > 0);
  }

  function historyDuration(seconds, padded = false) {
    const safe = Number.isFinite(Number(seconds)) ? Math.max(0, Math.floor(Number(seconds))) : 0;
    const digits = padded ? { minimumIntegerDigits: 2 } : undefined;
    return t('history.duration', { minutes: i18n.number(Math.floor(safe / 60), digits), seconds: i18n.number(safe % 60, digits) });
  }

  function renderHistoryBuffer() {
    const buffer = state.status?.history;
    const known = state.statusFresh && Number.isFinite(buffer?.combined_audio_secs);
    const current = known ? historyDuration(buffer.enabled ? buffer.combined_audio_secs : 0, true) : '—';
    const reading = byId('history-buffer-duration');
    if (reading.textContent !== current) reading.textContent = current;
    byId('history-buffer-capacity').textContent = Number.isFinite(buffer?.capacity_secs)
      ? t('history.buffer_capacity', { duration: historyDuration(buffer.capacity_secs) }) : '';
    const update = byId('history-buffer-update');
    update.textContent = t(!known ? 'history.buffer_unavailable' : buffer.enabled ? 'history.buffer_live' : 'history.buffer_disabled');
    update.dataset.live = String(Boolean(known && buffer.enabled));
  }

  function updateHistoryControls(unavailable, running) {
    const retention = byId('history-duration-minutes');
    const capacity = historySeconds(retention);
    const retentionError = !Number.isInteger(capacity) || capacity < 1 || capacity > 3600 ? t('history.retention_invalid') : '';
    retention.setCustomValidity(retentionError);
    byId('history-retention-error').textContent = retentionError;
    byId('history-retention-error').hidden = !retentionError;
    const include = byId('history-include');
    const request = byId('history-request-minutes');
    include.disabled = unavailable || running || (!include.checked && !historySelectionAvailable());
    request.disabled = unavailable || running || !include.checked;
    request.max = Number.isInteger(capacity) && capacity > 0 ? String(capacity / 60) : '60';
    const seconds = historySeconds(request);
    const requestError = include.checked && (!Number.isInteger(seconds) || seconds < 1 || seconds > capacity || seconds > 3600)
      ? t('history.request_invalid', { minutes: i18n.number(Number(request.max), { maximumFractionDigits: 4 }) }) : '';
    request.setCustomValidity(requestError);
    byId('history-request-error').textContent = requestError;
    byId('history-request-error').hidden = !requestError;
    byId('history-start-options').hidden = running || state.starting;
    byId('history-advanced-label').textContent = t(include.checked ? 'history.advanced_selected' : 'history.advanced');
    const duration = historyDuration;
    renderHistoryBuffer();
    const reason = !byId('history-enabled').checked || !state.status?.history?.enabled ? 'history.disabled_hint'
      : !byId('recording-enabled').checked && !byId('transcription-enabled').checked ? 'history.features_hint'
      : !historySelectionAvailable() ? 'history.empty_hint' : 'history.partial_hint';
    byId('history-availability-hint').textContent = t(reason);
    const included = Number(state.status?.history_included_secs);
    byId('history-session-status').hidden = !running || !(included > 0);
    const sessionHistoryLabel = included > 0 ? t(state.status?.history_transcription_pending ? 'history.transcribing' : 'history.included', { duration: duration(included) }) : '';
    if (byId('history-session-status').textContent !== sessionHistoryLabel) byId('history-session-status').textContent = sessionHistoryLabel;
  }

  function updateSessionFeatures(disabled) {
    const selected = routeNames.filter(route => byId(`${route}-enabled`).checked);
    if (selected.length) translationSelection = selected;
    byId('session-translation-enabled').checked = selected.length > 0;
    const scope = translationSelection.length === 2 ? 'both' : translationSelection[0];
    byId('session-translation-scope').textContent = t(`session.features_${scope}`);
    for (const feature of ['recording', 'transcription']) {
      byId(`session-${feature}-enabled`).checked = byId(`${feature}-enabled`).checked;
    }
    for (const control of document.querySelectorAll('[data-session-feature]')) control.disabled = disabled;
  }

  function updateControls() {
    const running = Boolean(state.status?.running);
    const processingSelected = routeNames.some(route => byId(`${route}-enabled`).checked) || byId('transcription-enabled').checked || byId('recording-enabled').checked;
    const unavailable = state.busy || state.syncing || !state.authenticated || !state.config || !state.status;
    updateSessionFeatures(running || unavailable || state.starting || state.configConflict);
    updateHistoryControls(unavailable, running);
    byId('interface-language').disabled = state.busy || state.syncing || !state.authenticated;
    byId('settings').disabled = running || unavailable;
    byId('session-name').disabled = running || unavailable;
    byId('save').disabled = running || unavailable || (!state.dirty && state.status?.local_runtime?.phase !== 'error') || state.configConflict;
    byId('start').disabled = running || unavailable || state.configConflict || !processingSelected;
    byId('reload-config').disabled = state.busy || state.syncing || !state.authenticated;
    byId('start').hidden = running || state.starting;
    byId('cancel-start').hidden = !state.starting;
    byId('cancel-start').disabled = state.cancelStarting || !state.authenticated;
    byId('stop').hidden = !running;
    byId('stop').disabled = state.busy || !state.authenticated;
    byId('install-devices').disabled = running || unavailable || !managesVirtualDevices();
    byId('uninstall-devices').disabled = running || unavailable || !managesVirtualDevices();
    byId('refresh-devices').disabled = state.busy || !state.authenticated;
    const startupUnavailable = state.busy || !state.authenticated || !state.autostart?.supported || !supportsHostAutostart();
    byId('autostart-enabled').disabled = startupUnavailable;
    byId('autostart-apply').disabled = startupUnavailable || byId('autostart-enabled').checked === state.autostart?.enabled;
    const transcriptionEnabled = byId('transcription-enabled').checked;
    for (const field of ['microphone', 'speaker', 'timestamps']) byId(`transcription-${field}`).disabled = !transcriptionEnabled;
    for (const field of ['microphone', 'speaker']) byId(`recording-${field}`).disabled = !byId('recording-enabled').checked;
    const recordingEnabled = byId('recording-enabled').checked;
    for (const route of routeNames) byId(`recording-${route}-gain`).disabled = !recordingEnabled || !byId(`recording-${route}`).checked;
    const mixedRecording = recordingEnabled && routeNames.every(route => byId(`recording-${route}`).checked);
    byId('recording-microphone-priority').disabled = !mixedRecording;
    for (const field of ['ducking', 'microphone-threshold']) byId(`recording-${field}`).disabled = !mixedRecording || !byId('recording-microphone-priority').checked;
    byId('save-state').textContent = state.configConflict ? t("ui.reload_settings") : state.dirty ? t("ui.unsaved_settings") : t("ui.settings_saved");
    byId('save-state').classList.toggle('dirty', state.dirty || state.configConflict);
    byId('session-hint').textContent = !state.authenticated
      ? t("ui.reopen_the_full_address_shown_in_the_terminal_to_connect_this_dashboard")
      : state.configConflict
        ? t("ui.settings_changed_outside_this_dashboard_reload_saved_settings_before_saving")
      : running
        ? t("ui.session_active_end_it_to_change_settings_physical_devices_can_be_switched_f")
      : state.status?.local_runtime?.phase === 'preparing'
        ? t('local.preparing_session')
      : !processingSelected
        ? t("ui.original_audio_routing_works_without_a_session_enable_translation_transcrip")
        : state.dirty
          ? t("ui.you_have_unsaved_settings_starting_the_session_also_saves_them")
          : t("ui.choose_which_features_to_use_and_start_a_session_to_translate_transcribe_or");
  }

  function renderStatus(status, fresh = true) {
    state.status = status;
    state.statusFresh = fresh;
    renderLocalRuntime();
    const sessionIdentity = status.session_name || status.session_id;
    const sessionLabel = sessionIdentity ? t(status.running ? 'session.identity' : 'session.previous', { name: sessionIdentity }) : '';
    if (byId('session-identity').textContent !== sessionLabel) byId('session-identity').textContent = sessionLabel;
    byId('session-identity').hidden = !sessionIdentity;
    const waiting = routeNames.every(route => routeWaiting(route, status));
    byId('session-state').textContent = status.running ? t("ui.session_active") : waiting ? t('routing.waiting_for_app') : status.routing_active ? t("ui.original_audio") : t("ui.no_routing");
    byId('status-dot').className = `status-dot${status.running || status.routing_active ? ' running' : ''}${status.last_error || status.routing_error ? ' error' : ''}`;
    byId('session-error').textContent = status.last_error || '';
    byId('session-error').hidden = !status.last_error;
    byId('routing-error').textContent = status.routing_error ? t('routing.error', { error: status.routing_error }) : '';
    byId('routing-error').hidden = !status.routing_error;
    const translations = { stopped: t("ui.stopped"), unconfigured: t('routing.unconfigured'), passthrough: t("ui.original_audio"), transcribing: t("ui.transcribing_original_audio"), starting: t("ui.starting"), connecting: t("ui.connecting_to_ai"), running: t("ui.running"), streaming: t("ui.streaming"), translating: t("ui.translating"), reconnecting: t("ui.reconnecting"), error: t("ui.error"), failed: t("ui.failed"), disabled: t("ui.disabled") };
    for (const route of routeNames) {
      const current = status[route];
      if (!current) continue;
      byId(`${route}-state`).textContent = current.device_error ? t("ui.device_unavailable") : routeWaiting(route, status) ? t('routing.waiting_for_app') : translations[current.state] || current.state || t("ui.stopped");
      byId(`${route}-state`).title = routeWaiting(route, status) ? waitingHint(route) : '';
      byId(`${route}-reconnects`).textContent = t('audio.reconnections', { count: i18n.number(current.reconnects || 0) });
      for (const metric of ['captured_frames', 'dropped_frames', 'processing_dropped_frames', 'underruns']) {
        byId(`${route}-${metric}`).textContent = i18n.number(current[metric] || 0);
        byId(`${route}-${metric}`).classList.toggle('nonzero', metric !== 'captured_frames' && current[metric] > 0);
      }
      for (const side of ['input', 'output']) {
        const raw = routeActive(route, status) ? Number(current[`${side}_level`]) || 0 : 0;
        const level = Math.max(0, Math.min(1, raw));
        byId(`${route}-${side}`).value = level;
        byId(`${route}-${side}-db`).textContent = raw > 0.0001 ? `${i18n.number(20 * Math.log10(raw), { maximumFractionDigits: 0 })} dB` : '−∞ dB';
      }
      const transcript = current.last_input_transcript;
      byId(`${route}-transcripts`).hidden = !transcript;
      byId(`${route}-input-transcript`).textContent = transcript || t("ui.waiting_for_audio");
    }
    renderSignalPaths();
    updateControls();
  }

  async function pollStatus() {
    if (!state.authenticated || state.polling || (state.busy && !state.starting) || state.syncing || document.hidden) return;
    state.polling = true;
    const activity = state.activity;
    try {
      const status = await api('/status');
      if ((state.busy && !state.starting) || state.activity !== activity) return;
      renderStatus(status);
      if (!state.starting) await synchronizeConfig(status);
    } catch (error) {
      state.statusFresh = false;
      renderHistoryBuffer();
      byId('session-state').textContent = t("ui.disconnected");
      byId('status-dot').className = 'status-dot error';
      showError(t('error.connection', { error: error.message }));
    } finally { state.polling = false; }
  }

  async function action(operation) {
    if (state.busy || state.syncing) return;
    state.busy = true;
    state.activity++;
    updateControls();
    showError('');
    announce('');
    try { await operation(); }
    catch (error) { showError(error.name === 'TimeoutError' ? t("ui.babel_took_too_long_to_respond_check_the_terminal_and_try_again") : error.message); }
    finally { state.busy = false; updateControls(); }
  }

  async function saveConfig() {
    if (state.configConflict || state.configRevision === null) throw new Error(t("ui.reload_saved_settings_before_continuing"));
    const config = collectConfig();
    const saved = await api('/config', { method: 'PUT', body: config, revision: state.configRevision, snapshot: true });
    state.configRevision = saved.revision;
    state.config = config;
    state.dirty = false;
    updateControls();
  }

  form.addEventListener('submit', (event) => event.preventDefault());
  document.querySelectorAll('[data-session-feature]').forEach(control => control.addEventListener('input', () => {
    // These are shortcuts to the canonical fields, not extra serialized settings.
    if (control.disabled) { updateControls(); return; }
    const feature = control.dataset.sessionFeature;
    if (feature === 'translation') {
      const selected = routeNames.filter(route => byId(`${route}-enabled`).checked);
      if (selected.length) translationSelection = selected;
      // Set both before publishing the edit, preserving a single-direction draft.
      for (const route of routeNames) byId(`${route}-enabled`).checked = control.checked && translationSelection.includes(route);
      byId('microphone-enabled').dispatchEvent(new Event('input', { bubbles: true }));
    } else {
      const field = byId(`${feature}-enabled`);
      field.checked = control.checked;
      field.dispatchEvent(new Event('input', { bubbles: true }));
    }
  }));
  form.addEventListener('input', (event) => {
    if (!event.target.matches('[data-field]')) return;
    state.dirty = true;
    updateGainLabels();
    if (['provider', 'enabled', 'engine', 'model'].includes(event.target.dataset.field) || event.target.dataset.section === 'transcription' || event.target.dataset.transcriptionProfile || event.target.dataset.profile === 'local') updateProviderControls();
    if (event.target.dataset.section === 'local_runtime') validateLocalDirectory();
    if (event.target.id === 'audio-quality') updateQualityHint();
    if (['files-base_path', 'transcription-directory', 'recording-directory'].includes(event.target.id)) scheduleFilePathPreview();
    updateControls();
  });
  // Some select controls emit change without input in older embedded browsers.
  form.addEventListener('change', (event) => {
    if (event.target.matches('select')) event.target.dispatchEvent(new Event('input', { bubbles: true }));
  });
  function reportWorkspaceValidity(container) {
    return window.BabelWorkspace ? window.BabelWorkspace.reportValidity(container) : container.reportValidity();
  }
  byId('save').addEventListener('click', () => {
    if (!reportWorkspaceValidity(form)) return;
    action(async () => { await saveConfig(); announce(t("ui.settings_saved_2")); });
  });
  byId('start').addEventListener('click', () => {
    if (state.configConflict || state.configRevision === null) return;
    if (!reportWorkspaceValidity(form)) return;
    if (!reportWorkspaceValidity(byId('session-name'))) return;
    if (byId('history-include').checked && !historySelectionAvailable()) {
      byId('history-start-options').open = true;
      showError(t('history.unavailable_error'));
      return;
    }
    if (!reportWorkspaceValidity(byId('history-request-minutes'))) return;
    const historySecondsRequested = byId('history-include').checked ? historySeconds(byId('history-request-minutes')) : 0;
    const name = byId('session-name').value.trim();
    action(async () => {
      if (state.dirty) await saveConfig();
      state.starting = true;
      state.startCancelled = false;
      updateControls();
      try { await api('/start', { method: 'POST', revision: state.configRevision, timeout: 1800000, body: { ...(name ? { name } : {}), history_seconds: historySecondsRequested } }); }
      catch (error) { if (!state.startCancelled) throw error; }
      finally { state.starting = false; }
      if (state.startCancelled) return;
      byId('history-include').checked = false;
      byId('history-start-options').open = false;
      renderStatus(await api('/status'));
      announce(t("ui.session_started_with_the_selected_features_original_audio_continues_on_rout"));
    });
  });
  for (const id of ['history-include', 'history-request-minutes']) byId(id).addEventListener('input', updateControls);
  byId('history-settings-link').addEventListener('click', () => { byId('history-start-options').open = false; });
  byId('history-start-options').addEventListener('keydown', event => {
    if (event.key !== 'Escape') return;
    event.preventDefault();
    byId('history-start-options').open = false;
    byId('history-start-options').querySelector('summary').focus();
  });
  byId('cancel-start').addEventListener('click', async () => {
    if (!state.starting || state.cancelStarting) return;
    state.cancelStarting = true;
    state.startCancelled = true;
    updateControls();
    try {
      await api('/stop', { method: 'POST' });
      renderStatus(await api('/status'));
      announce(t('local.start_cancelled'));
    } catch (error) { state.startCancelled = false; showError(error.message); }
    finally { state.cancelStarting = false; updateControls(); }
  });
  byId('stop').addEventListener('click', () => action(async () => {
    await api('/stop', { method: 'POST' });
    renderStatus(await api('/status'));
    announce(t("ui.session_ended_original_audio_passes_through_the_configured_devices_again"));
  }));
  byId('reload-config').addEventListener('click', () => action(async () => { await reloadConfig(); announce(t("ui.saved_settings_reloaded_review_devices_before_starting")); }));
  byId('refresh-devices').addEventListener('click', () => action(async () => { await Promise.all([refreshDevices(), refreshPlatform()]); announce(t("ui.device_list_refreshed")); }));
  function renderAutostart(status) {
    state.autostart = status;
    byId('autostart-enabled').checked = Boolean(status.enabled);
    renderAutostartDescription();
    updateControls();
  }
  function renderAutostartDescription() {
    const status = state.autostart;
    if (!status) return;
    const parts = [!status.supported ? t('autostart.unsupported') : status.enabled ? t('ui.autostart_enabled_babel_opens_with_local_original_audio_routing_without_sta') : t('ui.autostart_disabled')];
    const methods = { systemd_user: 'autostart.method_systemd_user', xdg: 'autostart.method_xdg', launch_agent: 'autostart.method_launch_agent', registry_run: 'autostart.method_registry_run' };
    if (status.supported && Object.hasOwn(methods, status.method)) {
      parts.push(t(methods[status.method]));
      if (typeof status.entry_path === 'string' && status.entry_path) parts.push(t('autostart.entry_path', { path: status.entry_path }));
    }
    byId('autostart-status').textContent = parts.join(' ');
  }
  async function refreshAutostart() { renderAutostart(await api('/autostart')); }
  byId('autostart-enabled').addEventListener('change', updateControls);
  byId('autostart-apply').addEventListener('click', () => {
    const enabled = byId('autostart-enabled').checked;
    action(async () => {
      renderAutostart(await api('/autostart', { method: 'POST', body: { enabled } }));
      announce(enabled ? t("ui.autostart_enabled_babel_will_open_in_the_tray_with_original_audio_without_s") : t("ui.autostart_disabled"));
    });
  });
  for (const operation of ['install', 'uninstall']) {
    byId(`${operation}-devices`).addEventListener('click', () => action(async () => {
      if (!managesVirtualDevices()) throw new Error(t('platform.management_unavailable'));
      const result = await api(`/virtual/${operation}`, { method: 'POST' });
      announce(result.message || t("ui.devices_updated"));
      await refreshDevices();
    }));
  }
  addEventListener('beforeunload', (event) => { if (state.dirty) event.preventDefault(); });
  document.addEventListener('visibilitychange', () => { if (!document.hidden) pollStatus(); });

  function profileChanged() {
    const provider = byId('profile-selector').value;
    for (const name of ['gemini', 'openai', 'local']) byId(`profile-${name}`).hidden = name !== provider;
    refreshCredentialStatus(provider);
  }

  async function refreshCredentialStatus(provider, stt = false) {
    if ((!stt && provider === 'local') || !state.authenticated) return;
    if (stt && provider === 'whisper' && managedEndpoint('stt-profile-whisper-endpoint')) return;
    const value = stt ? sttProfileValue : profileValue;
    const prefix = stt ? 'stt-' : '';
    const environment = value(provider, 'api_key_env');
    const status = byId(`${prefix}credential-${provider}-status`);
    if (!environment) {
      if (stt) i18n.message(status, t(provider === 'whisper' ? 'stt.optional_credential' : 'stt.credential_name_required'));
      return;
    }
    try {
      const result = await api(`/credentials?${new URLSearchParams({ api_key_env: environment })}`);
      if (value(provider, 'api_key_env') !== environment) return;
      i18n.message(status, result.configured
        ? t("ui.a_key_is_available_for_this_profile_in_memory_or_the_environment_its_value_")
        : t("ui.no_key_is_available_apply_a_temporary_key_or_set_the_environment_variable_b"));
    } catch (error) { if (value(provider, 'api_key_env') === environment) status.textContent = error.message; }
  }

  byId('profile-selector').addEventListener('change', profileChanged);
  for (const provider of ['gemini', 'openai']) {
    byId(`profile-${provider}-api_key_env`).addEventListener('change', () => refreshCredentialStatus(provider));
  }
  function sttProfileChanged() {
    const provider = byId('stt-profile-selector').value;
    for (const name of ['gemini', 'openai', 'deepgram', 'whisper']) byId(`stt-profile-${name}`).hidden = name !== provider;
    refreshCredentialStatus(provider, true);
  }
  byId('stt-profile-selector').addEventListener('change', sttProfileChanged);
  for (const provider of ['gemini', 'openai', 'deepgram', 'whisper']) {
    byId(`stt-profile-${provider}-api_key_env`).addEventListener('change', () => refreshCredentialStatus(provider, true));
  }
  document.querySelectorAll('[data-stt-route-profile]').forEach(button => button.addEventListener('click', () => {
    const field = byId(`stt-profile-${sttProvider(button.dataset.sttRouteProfile)}-model`);
    window.BabelWorkspace?.revealField(field);
  }));
  document.querySelectorAll('.credential-apply, .stt-credential-apply').forEach(button => button.addEventListener('click', () => action(async () => {
    const provider = button.dataset.provider;
    const stt = button.classList.contains('stt-credential-apply');
    const environment = (stt ? sttProfileValue : profileValue)(provider, 'api_key_env');
    if (!environment.trim()) throw new Error(t('stt.credential_name_required'));
    const input = byId(`${stt ? 'stt-' : ''}credential-${provider}`);
    const key = input.value;
    input.value = '';
    if (!key.trim()) throw new Error(t("ui.enter_a_key_for_this_app_instance"));
    await api('/credentials', { method: 'POST', body: { api_key_env: environment, key } });
    await refreshCredentialStatus(provider, stt);
    announce(t("ui.key_applied_to_this_app_instance_s_memory_only_it_will_be_discarded_when_ba"));
  })));
  document.querySelectorAll('.credential-clear, .stt-credential-clear').forEach(button => button.addEventListener('click', () => action(async () => {
    const provider = button.dataset.provider;
    const stt = button.classList.contains('stt-credential-clear');
    const environment = (stt ? sttProfileValue : profileValue)(provider, 'api_key_env');
    if (!environment.trim()) throw new Error(t('stt.credential_name_required'));
    byId(`${stt ? 'stt-' : ''}credential-${provider}`).value = '';
    await api('/credentials/clear', { method: 'POST', body: { api_key_env: environment } });
    await refreshCredentialStatus(provider, stt);
    announce(t("ui.temporary_key_removed_a_key_set_in_the_environment_remains_available_as_a_f"));
  })));

  function renderInterfaceText() {
    const metadata = state.interface;
    if (!metadata) return;
    const select = byId('interface-language');
    select.value = metadata.language;
    const languageName = metadata.languages.find(item => item.code === i18n.language)?.name || i18n.language;
    byId('interface-language-status').textContent = t('interface.using', { language: languageName });
    // Update labels in place so drafts, focus and native select values remain
    // attached to the same nodes during a language change.
    for (const option of document.querySelectorAll('[data-device-direction] option')) {
      const name = option.dataset.deviceName;
      option.textContent = !option.value ? t('ui.select_a_device')
        : option.dataset.deviceUnavailable === 'true' ? t('device.unavailable', { name })
        : option.dataset.deviceVirtual === 'true' ? t('device.virtual', { name }) : name;
    }
    for (const link of document.querySelectorAll('a[href^="/help/"]')) {
      link.hreflang = 'en';
      link.title = t('help.documentation');
    }
    renderPlatform();
    renderFilePathPreview();
    if (state.config) {
      updateProviderControls(); updateGainLabels(); updateQualityHint();
      if (state.status) renderStatus(state.status, state.statusFresh);
      renderAutostartDescription();
      updateControls();
    }
  }

  async function applyInterface(metadata) {
    state.interface = metadata;
    const select = byId('interface-language');
    for (const item of metadata.languages) {
      if (![...select.options].some(option => option.value === item.code)) select.add(new Option(item.name, item.code));
    }
    await i18n.setLanguage(metadata.resolved_language);
    renderInterfaceText();
  }

  async function refreshInterface() { await applyInterface(await api('/interface')); }

  byId('interface-language').addEventListener('change', () => {
    const language = byId('interface-language').value;
    action(async () => {
      try {
        const metadata = await api('/interface', { method: 'PUT', body: { language }, revision: state.configRevision });
        state.config.interface = { ...state.config.interface, language: metadata.language };
        state.configRevision = String(metadata.config_revision);
        await applyInterface(metadata);
        announce(t('interface.saved'));
      } finally { byId('interface-language').value = state.interface?.language || state.config?.interface?.language || 'system'; }
    });
  });

  async function initialize() {
    try { await i18n.setLanguage('en'); }
    catch (error) { showError(error.message); return; }

    if (!token || !/^[a-f0-9]{64}$/.test(token)) {
      showError(t("ui.to_connect_securely_open_the_full_babel_dashboard_link_shown_in_the_termina"));
      byId('session-state').textContent = t("ui.dashboard_disconnected");
      updateControls();
      return;
    }
    try {
      const snapshot = await api('/config', { snapshot: true });
      state.configRevision = snapshot.revision;
      state.authenticated = true;
      applyConfig(snapshot.body);
      await refreshInterface();
      const results = await Promise.allSettled([refreshDevices(), api('/status'), refreshAutostart(), refreshPlatform()]);
      if (results[0].status === 'rejected') showError(results[0].reason.message);
      if (results[1].status === 'fulfilled') { renderStatus(results[1].value); await synchronizeConfig(results[1].value); }
      else showError(results[1].reason.message);
      if (results[2].status === 'rejected') byId('autostart-status').textContent = t('autostart.read_error', { error: results[2].reason.message });
      updateControls();
      refreshCredentialStatus(byId('profile-selector').value);
      refreshCredentialStatus(byId('stt-profile-selector').value, true);
    } catch (error) {
      showError(error.message);
      byId('session-state').textContent = t("ui.dashboard_disconnected");
      updateControls();
    }
  }
  initialize();
  setInterval(pollStatus, 1000);
})();
